use crate::*;

/// Network retry policy. `attempts` counts the first try, so `1` disables
/// retries. Backoff grows as `base * 2^(n-1)`, capped at `max`.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RetryPolicy {
    pub(crate) attempts: u32,
    pub(crate) base_backoff_ms: u64,
    pub(crate) max_backoff_ms: u64,
    pub(crate) retry_statuses: Vec<u16>,
    pub(crate) retry_transport: bool,
}

impl RetryPolicy {
    /// `attempt` is the zero-based index of the try that just failed.
    pub(crate) fn should_retry(&self, attempt: u32) -> bool {
        attempt.saturating_add(1) < self.attempts
    }

    pub(crate) fn retry_status(&self, status: u16) -> bool {
        self.retry_statuses.contains(&status)
    }

    /// Sleeps the exponential backoff without crossing `deadline`. Returns
    /// false when the budget is exhausted, so the caller stops retrying.
    pub(crate) async fn backoff(&self, attempt: u32, deadline: time::Instant) -> bool {
        let left = deadline.saturating_duration_since(time::Instant::now());
        if left.is_zero() {
            tracing::debug!(
                attempt = attempt + 1,
                remaining_ms = 0,
                "retry budget exhausted"
            );
            return false;
        }
        let delay = self.delay(attempt).min(left);
        tracing::debug!(
            attempt = attempt + 1,
            delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            remaining_ms = u64::try_from(left.as_millis()).unwrap_or(u64::MAX),
            "retry backoff"
        );
        time::sleep(delay).await;
        true
    }

    pub(crate) fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u64 << attempt.min(16);
        let millis = self
            .base_backoff_ms
            .saturating_mul(factor)
            .min(self.max_backoff_ms);
        Duration::from_millis(millis)
    }
}

impl RetryPolicy {
    pub(crate) fn llm() -> Self {
        Self {
            attempts: 3,
            base_backoff_ms: 400,
            max_backoff_ms: 2500,
            retry_statuses: vec![408, 429, 500, 502, 503, 504],
            retry_transport: true,
        }
    }

    pub(crate) fn read_url() -> Self {
        Self {
            attempts: 2,
            base_backoff_ms: 200,
            max_backoff_ms: 2000,
            retry_statuses: vec![408, 429, 500, 502, 503, 504],
            retry_transport: true,
        }
    }

    pub(crate) fn search() -> Self {
        Self {
            attempts: 2,
            base_backoff_ms: 150,
            max_backoff_ms: 1000,
            retry_statuses: vec![429, 500, 502, 503, 504],
            retry_transport: true,
        }
    }
}
