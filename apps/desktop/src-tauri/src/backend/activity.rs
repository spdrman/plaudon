//! The in-app activity history (spec §5.5, §22(13)).
//!
//! An entry is written in the **same transaction** that commits the work it
//! describes, so the history is complete even when notifications are disabled,
//! denied or unsupported. Delivery is recorded afterwards by the notification
//! service; until it reports, an entry says exactly that and claims nothing.

use async_trait::async_trait;
use plaudon_domain::error::AppError;
use plaudon_domain::time::Timestamp;
use sqlx::{Row, SqliteConnection};

use crate::backend::store::{as_u64, rfc3339, storage_error};
use crate::backend::LocalBackend;
use crate::dto::{
    ActivityEntry, ActivityPage, ActivityTarget, ActivityTargetKind, NotificationCategory,
};
use crate::error::internal;
use crate::ports::ActivityPort;
use crate::validate::{PageCursor, ValidatedListActivity};

/// Reason stored while no notification attempt has been reported yet.
pub const NOT_ATTEMPTED: &str = "delivery_not_attempted";

/// Everything needed to append one activity entry.
#[derive(Debug, Clone)]
pub struct ActivityDraft {
    /// Category this entry would notify under.
    pub category: NotificationCategory,
    /// When the underlying change was committed.
    pub occurred_at: Timestamp,
    /// Short title.
    pub title: String,
    /// Privacy-safe body.
    pub body: String,
    /// Navigation target.
    pub target: Option<ActivityTarget>,
}

/// Append an entry inside the caller's transaction.
pub async fn record_in_tx(
    tx: &mut SqliteConnection,
    draft: &ActivityDraft,
) -> Result<String, AppError> {
    let id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO activity_events (id, category, occurred_at, title, body, \
         notification_delivered, notification_suppressed_reason, target_kind, target_id, read) \
         VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, 0)",
    )
    .bind(&id)
    .bind(draft.category.as_str())
    .bind(draft.occurred_at.as_millis())
    .bind(&draft.title)
    .bind(&draft.body)
    .bind(NOT_ATTEMPTED)
    .bind(draft.target.as_ref().map(|target| match target.kind {
        ActivityTargetKind::Recording => "recording",
        ActivityTargetKind::Job => "job",
        ActivityTargetKind::Settings => "settings",
    }))
    .bind(draft.target.as_ref().map(|target| target.id.clone()))
    .execute(&mut *tx)
    .await
    .map_err(|err| storage_error("cannot append an activity entry", &err))?;
    Ok(id)
}

#[async_trait]
impl ActivityPort for LocalBackend {
    async fn list_activity(&self, query: ValidatedListActivity) -> Result<ActivityPage, AppError> {
        let limit = i64::from(query.limit);
        let offset = i64::try_from(query.cursor.offset())
            .map_err(|_| internal("activity cursor offset is out of range"))?;
        let rows = sqlx::query(
            "SELECT id, category, occurred_at, title, body, notification_delivered, \
             notification_suppressed_reason, target_kind, target_id, read \
             FROM activity_events ORDER BY occurred_at DESC, id DESC LIMIT ?1 OFFSET ?2",
        )
        .bind(limit + 1)
        .bind(offset)
        .fetch_all(self.pool())
        .await
        .map_err(|err| storage_error("cannot list activity", &err))?;

        let has_more = rows.len() > query.limit as usize;
        let mut items = Vec::with_capacity(rows.len().min(query.limit as usize));
        for row in rows.iter().take(query.limit as usize) {
            let category: String = row.get("category");
            let target_kind: Option<String> = row.get("target_kind");
            let target_id: Option<String> = row.get("target_id");
            let target = match (target_kind.as_deref(), target_id) {
                (Some("recording"), Some(id)) => Some(ActivityTarget {
                    kind: ActivityTargetKind::Recording,
                    id,
                }),
                (Some("job"), Some(id)) => Some(ActivityTarget {
                    kind: ActivityTargetKind::Job,
                    id,
                }),
                (Some("settings"), Some(id)) => Some(ActivityTarget {
                    kind: ActivityTargetKind::Settings,
                    id,
                }),
                _ => None,
            };
            items.push(ActivityEntry {
                activity_id: row.get("id"),
                category: NotificationCategory::parse(&category)
                    .ok_or_else(|| internal("stored activity category is unknown"))?,
                occurred_at: rfc3339("activity.occurred_at", row.get("occurred_at"))?,
                title: row.get("title"),
                body: row.get("body"),
                notification_delivered: row.get::<i64, _>("notification_delivered") != 0,
                notification_suppressed_reason: row.get("notification_suppressed_reason"),
                target,
                read: row.get::<i64, _>("read") != 0,
            });
        }

        let next_cursor = has_more
            .then(|| PageCursor::at(query.cursor.offset() + u64::from(query.limit)).encode());
        let unread: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM activity_events WHERE read = 0")
            .fetch_one(self.pool())
            .await
            .map_err(|err| storage_error("cannot count unread activity", &err))?;

        Ok(ActivityPage {
            items,
            next_cursor,
            unread_count: as_u64("unread count", unread)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::testing::backend_fixture;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn entries_page_newest_first_and_report_no_delivery_until_one_is_recorded() {
        let fixture = backend_fixture().await;
        let mut conn = fixture.backend.pool().acquire().await.unwrap();
        for index in 0..3u8 {
            record_in_tx(
                &mut conn,
                &ActivityDraft {
                    category: NotificationCategory::ImportFinished,
                    occurred_at: Timestamp::from_millis(i64::from(index) * 1000),
                    title: format!("entry {index}"),
                    body: "body".into(),
                    target: None,
                },
            )
            .await
            .unwrap();
        }
        drop(conn);

        let page = fixture
            .backend
            .list_activity(ValidatedListActivity {
                cursor: PageCursor::START,
                limit: 2,
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].title, "entry 2");
        assert_eq!(page.unread_count, 3);
        assert!(!page.items[0].notification_delivered);
        assert_eq!(
            page.items[0].notification_suppressed_reason.as_deref(),
            Some(NOT_ATTEMPTED)
        );

        let cursor = page.next_cursor.clone().unwrap();
        let second = fixture
            .backend
            .list_activity(ValidatedListActivity {
                cursor: PageCursor::decode(&cursor).unwrap(),
                limit: 2,
            })
            .await
            .unwrap();
        assert_eq!(second.items.len(), 1);
        assert!(second.next_cursor.is_none());
    }
}
