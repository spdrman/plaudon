//! Retry policy and bounded concurrency (spec 10.6, 14.4).
//!
//! Only [`Retriability::AutomaticRetry`] failures are retried here. Anything
//! the provider may already have billed is returned to the caller, which
//! surfaces **Retry may charge again** and waits for an explicit decision
//! (spec 10.7). There is no provider or model substitution on any path
//! (spec 10.6).

use std::time::Duration;

use crate::error::{OpenRouterError, Retriability};

/// Bounded, jittered exponential backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Maximum number of *retries* after the first attempt.
    pub max_retries: u32,
    /// Delay before the first retry.
    pub base_delay: Duration,
    /// Upper bound on any single delay, before jitter.
    pub max_delay: Duration,
    /// Upper bound on a provider-supplied `Retry-After`. A provider asking for
    /// an hour parks the job instead of holding a worker.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            max_retry_after: Duration::from_secs(120),
        }
    }
}

/// What the engine should do after one failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Wait this long, then try again.
    RetryAfter(Duration),
    /// Stop. Either the failure is terminal or the budget of retries is spent.
    Stop,
    /// Stop and ask the user, because a retry may be billed twice.
    AskUser,
    /// Park the job: the provider asked for longer than we hold a worker.
    Park(Duration),
}

impl RetryPolicy {
    /// Decide what to do after `attempt` (1-based) failed with `error`.
    ///
    /// `jitter_ratio` is supplied by the caller in `0.0..=1.0` so the decision
    /// is a pure function and the bounds can be property-tested; production
    /// passes a random sample.
    #[must_use]
    pub fn decide(&self, attempt: u32, error: &OpenRouterError, jitter_ratio: f64) -> RetryDecision {
        match error.retriability() {
            Retriability::Terminal => return RetryDecision::Stop,
            Retriability::RetryNeedsCostAcknowledgement => return RetryDecision::AskUser,
            Retriability::AutomaticRetry => {}
        }
        if attempt > self.max_retries {
            return RetryDecision::Stop;
        }
        if let OpenRouterError::RateLimited {
            retry_after_seconds: Some(seconds),
        } = error
        {
            let asked = Duration::from_secs(u64::from(*seconds));
            // Honour `Retry-After` exactly, with no jitter: the provider named
            // a time and shortening it would be the one thing it asked us not
            // to do.
            return if asked > self.max_retry_after {
                RetryDecision::Park(asked)
            } else {
                RetryDecision::RetryAfter(asked)
            };
        }
        RetryDecision::RetryAfter(self.backoff(attempt, jitter_ratio))
    }

    /// The jittered delay for `attempt` (1-based).
    ///
    /// Full jitter in `[delay/2, delay]`: enough spread to decorrelate retries
    /// without ever waiting less than half the intended backoff.
    #[must_use]
    pub fn backoff(&self, attempt: u32, jitter_ratio: f64) -> Duration {
        let exponent = attempt.saturating_sub(1).min(16);
        let scaled = self
            .base_delay
            .saturating_mul(2u32.saturating_pow(exponent))
            .min(self.max_delay);
        let ratio = jitter_ratio.clamp(0.0, 1.0);
        let nanos = scaled.as_nanos() as f64;
        let jittered = nanos * (0.5 + 0.5 * ratio);
        Duration::from_nanos(jittered as u64)
    }
}

/// Sample a jitter ratio from the thread RNG.
#[must_use]
pub fn random_jitter() -> f64 {
    use rand::Rng;
    rand::thread_rng().gen_range(0.0..=1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_possibly_billed_failure_asks_the_user_instead_of_retrying() {
        let policy = RetryPolicy::default();
        for error in [
            OpenRouterError::MalformedResponse,
            OpenRouterError::TruncatedResponse,
        ] {
            assert_eq!(policy.decide(1, &error, 0.5), RetryDecision::AskUser);
        }
        assert_eq!(
            policy.decide(1, &OpenRouterError::OutcomeUnknown, 0.5),
            RetryDecision::Stop
        );
    }

    #[test]
    fn terminal_failures_never_retry() {
        let policy = RetryPolicy::default();
        for error in [
            OpenRouterError::Unauthorized,
            OpenRouterError::InsufficientCredit,
            OpenRouterError::UnknownModel,
            OpenRouterError::PayloadTooLarge,
            OpenRouterError::Rejected,
        ] {
            assert_eq!(policy.decide(1, &error, 0.5), RetryDecision::Stop);
        }
    }

    #[test]
    fn retry_after_is_honoured_exactly() {
        let policy = RetryPolicy::default();
        let decision = policy.decide(
            1,
            &OpenRouterError::RateLimited {
                retry_after_seconds: Some(42),
            },
            0.0,
        );
        assert_eq!(decision, RetryDecision::RetryAfter(Duration::from_secs(42)));
    }

    #[test]
    fn an_excessive_retry_after_parks_the_job() {
        let policy = RetryPolicy::default();
        let decision = policy.decide(
            1,
            &OpenRouterError::RateLimited {
                retry_after_seconds: Some(3_600),
            },
            0.0,
        );
        assert_eq!(decision, RetryDecision::Park(Duration::from_secs(3_600)));
    }

    #[test]
    fn the_retry_budget_is_finite() {
        let policy = RetryPolicy::default();
        assert_eq!(
            policy.decide(4, &OpenRouterError::ProviderUnavailable, 0.5),
            RetryDecision::Stop
        );
    }

    proptest! {
        #[test]
        fn backoff_stays_within_half_and_full_of_the_capped_delay(
            attempt in 1u32..10,
            ratio in 0.0f64..=1.0,
        ) {
            let policy = RetryPolicy::default();
            let delay = policy.backoff(attempt, ratio);
            let cap = policy.max_delay;
            prop_assert!(delay <= cap);
            prop_assert!(delay >= policy.base_delay / 2);
        }

        #[test]
        fn backoff_is_monotone_in_the_attempt_for_a_fixed_jitter(
            attempt in 1u32..8,
        ) {
            let policy = RetryPolicy::default();
            prop_assert!(policy.backoff(attempt, 1.0) <= policy.backoff(attempt + 1, 1.0));
        }
    }
}
