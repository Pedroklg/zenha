use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub base: Duration,
    pub cap: Duration,
    pub max_attempts: u32,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            cap: Duration::from_secs(15),
            max_attempts: 8,
        }
    }
}

impl ReconnectPolicy {
    pub fn delay(&self, attempt: u32) -> Duration {
        backoff_delay(attempt, self.base, self.cap)
    }
}

/// Delay before the given 1-based retry attempt: `base * 2^(attempt - 1)`, capped.
pub fn backoff_delay(attempt: u32, base: Duration, cap: Duration) -> Duration {
    let exponent = attempt.saturating_sub(1).min(20);
    base.saturating_mul(1u32 << exponent).min(cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_doubles_and_is_capped() {
        let policy = ReconnectPolicy::default();
        let secs: Vec<u64> = (1..=8).map(|n| policy.delay(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 15, 15, 15, 15]);
    }

    #[test]
    fn extreme_attempts_do_not_overflow() {
        let policy = ReconnectPolicy::default();
        assert_eq!(policy.delay(0), Duration::from_secs(1));
        assert_eq!(policy.delay(u32::MAX), policy.cap);
    }
}
