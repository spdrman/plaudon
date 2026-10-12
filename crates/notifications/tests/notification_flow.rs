//! End-to-end behaviour of the notification path, exercised through the public
//! API only (spec 5.5, 14.5, 22(13)).

use std::sync::Arc;
use std::time::Duration;

use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_domain::time::Timestamp;
use plaudon_notifications::testing::{Behavior, RecordingNotifier};
use plaudon_notifications::{
    privacy, AttentionReason, BudgetScope, DeliveryOutcome, Dispatcher, NotificationCategory,
    NotificationContext, NotificationSettings, NotificationTarget, OsPermission,
};

fn contexts() -> Vec<NotificationContext> {
    vec![
        NotificationContext::ImportFinished {
            imported: 3,
            failed: 1,
            recording: Some(RecordingId::new()),
        },
        NotificationContext::TranscriptionFinished {
            recordings: 1,
            recording: Some(RecordingId::new()),
        },
        NotificationContext::RequiresAttention {
            jobs: 2,
            reason: AttentionReason::AuthorizationRequired,
            job: Some(JobId::new()),
        },
        NotificationContext::CloudBudgetExceeded {
            scope: BudgetScope::Monthly,
            paused_jobs: 4,
        },
    ]
}

fn dispatcher(notifier: Arc<RecordingNotifier>, settings: NotificationSettings) -> Dispatcher {
    Dispatcher::new(notifier, settings).with_timeout(Duration::from_millis(150))
}

#[tokio::test]
async fn the_four_spec_categories_each_deliver_a_distinct_privacy_safe_payload() {
    let notifier = RecordingNotifier::granted();
    let dispatcher = dispatcher(Arc::clone(&notifier), NotificationSettings::uniform(true));

    for context in contexts() {
        let request = privacy::build(context, Timestamp::now());
        assert_eq!(
            dispatcher.notify(&request).await,
            DeliveryOutcome::Delivered
        );
    }

    let delivered = notifier.deliveries().await;
    assert_eq!(delivered.len(), 4);

    let mut categories: Vec<NotificationCategory> =
        delivered.iter().map(|request| request.category).collect();
    categories.sort_unstable();
    categories.dedup();
    assert_eq!(categories, NotificationCategory::ALL.to_vec());

    let mut bodies: Vec<&str> = delivered
        .iter()
        .map(|request| request.body.as_str())
        .collect();
    bodies.sort_unstable();
    bodies.dedup();
    assert_eq!(bodies.len(), 4, "categories must not share a body");

    for request in &delivered {
        assert!(!request.body.is_empty(), "every category needs a body");
        assert!(request.body.is_ascii());
        assert!(!request.body.contains('/'), "{:?}", request.body);
        assert!(
            request
                .body
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | ',' | '.' | '-')),
            "{:?} contains disallowed characters",
            request.body
        );
    }
}

#[tokio::test]
async fn turning_a_category_off_stops_delivery_without_touching_the_os() {
    let notifier = RecordingNotifier::granted();
    let dispatcher = dispatcher(Arc::clone(&notifier), NotificationSettings::uniform(true));
    let request = privacy::build(
        NotificationContext::ImportFinished {
            imported: 1,
            failed: 0,
            recording: None,
        },
        Timestamp::EPOCH,
    );

    assert_eq!(
        dispatcher.notify(&request).await,
        DeliveryOutcome::Delivered
    );

    dispatcher
        .set_settings(
            NotificationSettings::uniform(true)
                .with_category(NotificationCategory::ImportFinished, false),
        )
        .await;
    let before = notifier.permission_checks().await;

    assert_eq!(
        dispatcher.notify(&request).await,
        DeliveryOutcome::SuppressedByPreference
    );
    assert_eq!(notifier.deliveries().await.len(), 1);
    assert_eq!(
        notifier.permission_checks().await,
        before,
        "no OS call for an opted-out category"
    );
}

#[tokio::test]
async fn an_os_denial_never_hides_the_event_from_the_caller() {
    let notifier = RecordingNotifier::with_permission(OsPermission::Denied);
    let dispatcher = dispatcher(Arc::clone(&notifier), NotificationSettings::uniform(true));

    for context in contexts() {
        let request = privacy::build(context, Timestamp::EPOCH);
        let outcome = dispatcher.notify(&request).await;
        assert_eq!(outcome, DeliveryOutcome::SuppressedByOsPermission);
        // The caller still holds a complete, recordable event.
        assert!(!outcome.delivered());
        assert_eq!(
            outcome.suppression_reason(),
            Some("suppressed_by_os_permission")
        );
        assert!(
            !request.title.is_empty(),
            "the event still has a title to record"
        );
    }
    assert!(
        notifier.deliveries().await.is_empty(),
        "a denied OS never sees a payload"
    );
}

#[tokio::test]
async fn a_broken_backend_never_breaks_the_completion_path() {
    let request = privacy::build(
        NotificationContext::TranscriptionFinished {
            recordings: 2,
            recording: None,
        },
        Timestamp::EPOCH,
    );

    for behavior in [
        Behavior::Fail("Notification Center is unavailable".to_owned()),
        Behavior::Panic,
        Behavior::Hang(Duration::from_secs(30)),
    ] {
        let notifier = RecordingNotifier::granted().set_behavior(behavior).await;
        let dispatcher = dispatcher(Arc::clone(&notifier), NotificationSettings::uniform(true));

        let started = std::time::Instant::now();
        let outcome = dispatcher.notify(&request).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(matches!(outcome, DeliveryOutcome::Failed(_)), "{outcome:?}");
        assert_eq!(outcome.suppression_reason(), Some("failed"));
    }
}

#[tokio::test]
async fn delivered_payloads_carry_a_routable_target_token() {
    let recording = RecordingId::new();
    let notifier = RecordingNotifier::granted();
    let dispatcher = dispatcher(Arc::clone(&notifier), NotificationSettings::uniform(true));
    let request = privacy::build(
        NotificationContext::ImportFinished {
            imported: 1,
            failed: 0,
            recording: Some(recording),
        },
        Timestamp::EPOCH,
    );

    assert_eq!(
        dispatcher.notify(&request).await,
        DeliveryOutcome::Delivered
    );

    let delivered = notifier.deliveries().await;
    let token = delivered
        .first()
        .and_then(|request| request.target)
        .map(|target| target.activation_token())
        .unwrap_or_default();
    assert!(!token.contains(' '), "{token:?}");
    assert_eq!(
        NotificationTarget::from_activation_token(&token),
        Some(NotificationTarget::Recording(recording))
    );
}

/// The shape `set_notification_settings.request.schema.json` publishes, which
/// the deleted TypeScript contract used to be the second copy of.
#[tokio::test]
async fn settings_serialise_to_the_published_wire_shape() {
    let settings = NotificationSettings::uniform(false)
        .with_enabled(true)
        .with_category(NotificationCategory::RequiresAttention, true);

    let json = serde_json::to_value(&settings).unwrap_or(serde_json::Value::Null);
    assert_eq!(
        json,
        serde_json::json!({
            "enabled": true,
            "categories": {
                "import_finished": false,
                "transcription_finished": false,
                "requires_attention": true,
                "cloud_budget_exceeded": false,
            }
        })
    );

    let round_tripped: Option<NotificationSettings> = serde_json::from_value(json).ok();
    assert_eq!(round_tripped, Some(settings));
}
