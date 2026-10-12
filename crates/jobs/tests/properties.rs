//! Property tests for the job engine (SPEC v1.3 §18.2).
//!
//! Three families:
//! * random transition sequences never produce a false completion or two
//!   terminal states;
//! * for any enqueue sequence, the number of persisted jobs equals the number
//!   of distinct keys;
//! * the scheduler never exceeds a configured limit for any interleaving.

use std::collections::BTreeMap;

use plaudon_jobs::key::{
    ImportJobKey, JobKey, ProcessingJobKey, ProcessingStage, ReprocessToken, SourceKind,
};
use plaudon_jobs::scheduler::{
    preflight, PowerSource, PreflightDecision, ResourceClass, ResourceRequest, ResourceScheduler,
    SchedulerLimits, SystemResources,
};
use plaudon_jobs::state::{
    check_transition, is_legal_edge, Actor, GuardFacts, JobState, TransitionCause,
};
use plaudon_jobs::store::{EnqueueRequest, JobOutput, JobStore};
use proptest::prelude::*;

fn any_state() -> impl Strategy<Value = JobState> {
    prop::sample::select(JobState::ALL.to_vec())
}

fn any_actor() -> impl Strategy<Value = Actor> {
    prop::sample::select(vec![Actor::System, Actor::User, Actor::Recovery])
}

fn any_cause() -> impl Strategy<Value = TransitionCause> {
    (any_actor(), any::<bool>(), any::<bool>()).prop_map(|(actor, consent, cost)| {
        let mut cause = match actor {
            Actor::System => TransitionCause::system("prop"),
            Actor::User => TransitionCause::user("prop"),
            Actor::Recovery => TransitionCause::recovery("prop"),
        };
        cause.consent_verified = consent;
        cause.cost_acknowledged = cost;
        cause
    })
}

proptest! {
    /// Walking the machine with arbitrary requests never reaches `Completed`
    /// unless an output was committed, and never leaves a terminal state.
    #[test]
    fn random_walks_never_produce_a_false_completion(
        steps in prop::collection::vec((any_state(), any_cause(), any::<bool>()), 1..60)
    ) {
        let mut current = JobState::Queued;
        let mut terminal_entries = 0;
        for (target, cause, has_output) in steps {
            let facts = GuardFacts { has_committed_output: has_output };
            if let Ok(()) = check_transition(current, target, &cause, facts) {
                    prop_assert!(is_legal_edge(current, target));
                    if target == JobState::Completed {
                        prop_assert!(has_output, "Completed without a committed output row");
                    }
                    if target.is_terminal() {
                        terminal_entries += 1;
                    }
                current = target;
            }
            // A rejected transition leaves the state untouched.
            prop_assert!(terminal_entries <= 1, "entered a terminal state twice");
        }
    }

    /// A possibly-billed job never returns to dispatch without a deliberate,
    /// cost-acknowledged user action, whatever sequence precedes it.
    #[test]
    fn possibly_billed_work_is_never_auto_redispatched(cause in any_cause()) {
        let facts = GuardFacts::default();
        let result = check_transition(
            JobState::CloudOutcomeUnknown,
            JobState::CloudReady,
            &cause,
            facts,
        );
        if result.is_ok() {
            prop_assert_eq!(cause.actor, Actor::User);
            prop_assert!(cause.cost_acknowledged);
        }
    }

    /// A dispatch only ever starts with a verified consent grant.
    #[test]
    fn dispatch_always_requires_verified_consent(
        from in any_state(),
        cause in any_cause(),
        has_output in any::<bool>(),
    ) {
        let facts = GuardFacts { has_committed_output: has_output };
        if check_transition(from, JobState::CloudSending, &cause, facts).is_ok() {
            prop_assert!(cause.consent_verified);
            prop_assert_eq!(from, JobState::CloudReady);
        }
    }

    /// Key equality is exactly structural equality: equal keys digest equally,
    /// different keys never collide.
    #[test]
    fn distinct_processing_keys_never_collide(
        audio in "[0-9a-f]{64}",
        engine in "[a-z_]{1,12}",
        model in "[a-z0-9.-]{1,16}",
        version in prop::option::of("[0-9.]{1,8}"),
        template in prop::option::of("[a-z]{1,8}@[0-9]{1,3}"),
        privacy in prop::option::of("[a-z-]{1,12}"),
        resample in 8000u32..48001,
    ) {
        let mut config = BTreeMap::new();
        config.insert("resample_hz".to_string(), resample.to_string());
        let key = ProcessingJobKey {
            audio_sha256: audio.clone(),
            transcript_revision: None,
            stage: ProcessingStage::Transcribe,
            engine: engine.clone(),
            model: model.clone(),
            model_version: version.clone(),
            normalization_config: config.clone(),
            prompt_template_version: template.clone(),
            privacy_policy_snapshot: privacy.clone(),
            reprocess: None,
        };
        let same = key.clone();
        prop_assert_eq!(
            JobKey::Processing(key.clone()).digest(),
            JobKey::Processing(same).digest()
        );

        // Changing exactly one dimension always changes the digest.
        let mut other = key.clone();
        other.stage = ProcessingStage::Diarize;
        prop_assert_ne!(
            JobKey::Processing(key.clone()).digest(),
            JobKey::Processing(other).digest()
        );

        // An explicit reprocess is always distinct from the original.
        let rerun = key.clone().with_reprocess(ReprocessToken::new());
        prop_assert_ne!(
            JobKey::Processing(key).digest(),
            JobKey::Processing(rerun).digest()
        );
    }

    /// Preflight is total: it either admits or parks in a waiting state, and
    /// never produces a terminal state.
    #[test]
    fn preflight_never_fails_a_job(
        disk_needed in 0u64..1_000_000_000,
        ram_needed in 0u64..1_000_000_000,
        disk_free in 0u64..1_000_000_000,
        ram_free in 0u64..1_000_000_000,
        asset in any::<bool>(),
        source in any::<bool>(),
        saver in any::<bool>(),
        battery in any::<bool>(),
    ) {
        let decision = preflight(
            ResourceRequest {
                required_disk_bytes: disk_needed,
                estimated_ram_bytes: ram_needed,
                needs_model_asset: true,
                needs_source: true,
                heavy_local_compute: true,
            },
            SystemResources {
                available_disk_bytes: disk_free,
                available_ram_bytes: ram_free,
                power_source: if battery { PowerSource::Battery } else { PowerSource::Ac },
                power_saver_enabled: saver,
                model_asset_present: asset,
                source_available: source,
            },
        );
        match decision {
            PreflightDecision::Admit => {
                prop_assert!(asset && source);
                prop_assert!(disk_needed <= disk_free);
                prop_assert!(ram_needed <= ram_free);
            }
            PreflightDecision::Park { state, reason } => {
                prop_assert!(state.is_waiting());
                prop_assert!(!state.is_terminal());
                prop_assert_eq!(state, reason.wait_state());
            }
        }
    }
}

/// The scheduler never exceeds a configured limit for any interleaving.
#[test]
fn scheduler_respects_limits_under_random_interleavings() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime");

    let mut runner = proptest::test_runner::TestRunner::new(proptest::test_runner::Config {
        cases: 24,
        ..proptest::test_runner::Config::default()
    });
    runner
        .run(
            &(1u32..5, 1usize..24, prop::collection::vec(0u64..3, 1..24)),
            |(limit, tasks, delays)| {
                let scheduler = ResourceScheduler::new(
                    SchedulerLimits::default().with(ResourceClass::Cpu, limit),
                );
                runtime.block_on(async {
                    let mut handles = Vec::new();
                    for index in 0..tasks {
                        let scheduler = scheduler.clone();
                        let delay = delays[index % delays.len()];
                        handles.push(tokio::spawn(async move {
                            let token = tokio_util::sync::CancellationToken::new();
                            let admission = scheduler
                                .admit(
                                    ResourceClass::Cpu,
                                    plaudon_domain::ids::JobId::new(),
                                    &token,
                                )
                                .await
                                .expect("admit");
                            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                            drop(admission);
                        }));
                    }
                    for handle in handles {
                        handle.await.expect("task");
                    }
                });
                prop_assert!(
                    scheduler.high_water_mark(ResourceClass::Cpu) <= limit,
                    "high water {} exceeded limit {limit}",
                    scheduler.high_water_mark(ResourceClass::Cpu)
                );
                prop_assert_eq!(scheduler.in_flight(ResourceClass::Cpu), 0);
                Ok(())
            },
        )
        .expect("property");
}

/// For any enqueue sequence, the number of persisted jobs equals the number of
/// distinct keys (§18.2 import/idempotency properties).
#[test]
fn persisted_job_count_equals_distinct_key_count() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let mut runner = proptest::test_runner::TestRunner::new(proptest::test_runner::Config {
        cases: 16,
        ..proptest::test_runner::Config::default()
    });
    runner
        .run(
            &prop::collection::vec((0u8..6, any::<bool>()), 1..24),
            |requests| {
                runtime.block_on(async {
                    let dir = tempfile::tempdir().expect("tempdir");
                    let store = JobStore::open_at(dir.path().join("app.sqlite"))
                        .await
                        .expect("store");

                    let mut distinct = std::collections::HashSet::new();
                    let mut explicit_reprocesses = 0usize;
                    for (slot, reprocess) in &requests {
                        let mut key =
                            ImportJobKey::by_content(SourceKind::LocalFile, format!("{slot:064}"));
                        if *reprocess {
                            key = key.with_reprocess(ReprocessToken::new());
                            explicit_reprocesses += 1;
                        }
                        let job_key = JobKey::Import(key.clone());
                        distinct.insert(job_key.digest());
                        store
                            .enqueue(EnqueueRequest::new("import", key, ResourceClass::LocalDisk))
                            .await
                            .expect("enqueue");
                    }

                    let persisted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs")
                        .fetch_one(store.read_pool())
                        .await
                        .expect("count");
                    assert_eq!(
                        usize::try_from(persisted).expect("non-negative count"),
                        distinct.len(),
                        "one row per distinct key; {explicit_reprocesses} explicit reprocesses"
                    );
                });
                Ok(())
            },
        )
        .expect("property");
}

/// Every committed job output is reachable and a `Completed` job always has at
/// least one.
#[test]
fn completed_jobs_always_have_at_least_one_committed_output() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = JobStore::open_at(dir.path().join("app.sqlite"))
            .await
            .expect("store");
        for index in 0..8 {
            let key = ImportJobKey::by_content(SourceKind::LocalFile, format!("{index:064}"));
            let job = store
                .enqueue(EnqueueRequest::new("import", key, ResourceClass::LocalDisk))
                .await
                .expect("enqueue")
                .job_id();
            for state in [
                JobState::Preflight,
                JobState::RunningLocal,
                JobState::Verifying,
                JobState::Committing,
            ] {
                store
                    .transition(job, state, TransitionCause::system("advance"))
                    .await
                    .expect("advance");
            }
            if index % 2 == 0 {
                store
                    .complete(
                        job,
                        vec![JobOutput::new("media_asset", format!("sha256/{index}.wav"))],
                        TransitionCause::system("done"),
                    )
                    .await
                    .expect("complete");
            }
        }

        let orphaned: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM jobs j
             WHERE j.state = 'completed'
               AND NOT EXISTS (SELECT 1 FROM job_outputs o WHERE o.job_id = j.id)",
        )
        .fetch_one(store.read_pool())
        .await
        .expect("query");
        assert_eq!(orphaned, 0, "a completed job must always have outputs");
    });
}
