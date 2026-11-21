//! Keeps an audio stream alive: when the device fails (unplugged, Bluetooth profile switch,
//! driver reset), waits a little and opens it again until asked to stop.

use std::fmt::Display;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const STOP_CHECK: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy)]
pub(crate) struct Backoff {
    pub first: Duration,
    pub max: Duration,
    /// A stream that ran this long before failing starts again from `first`.
    pub healthy_after: Duration,
}

pub(crate) const DEVICE_BACKOFF: Backoff = Backoff {
    first: Duration::from_millis(500),
    max: Duration::from_secs(2),
    healthy_after: Duration::from_secs(5),
};

/// Runs `open_and_run` until it returns `Ok` (a requested stop) or `stop` is set.
pub(crate) fn run_reopening<E: Display>(
    what: &str,
    stop: &AtomicBool,
    backoff: Backoff,
    mut open_and_run: impl FnMut() -> Result<(), E>,
) {
    let mut delay = backoff.first;
    let mut failures: u32 = 0;
    loop {
        let started = Instant::now();
        let result = open_and_run();
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let err = match result {
            Ok(()) => {
                tracing::info!(what, "audio stream stopped");
                return;
            }
            Err(err) => err,
        };
        if started.elapsed() >= backoff.healthy_after {
            delay = backoff.first;
            failures = 0;
        }
        failures = failures.saturating_add(1);
        if failures == 1 {
            tracing::warn!(what, %err, "audio device failed; reopening");
        } else {
            tracing::debug!(what, %err, failures, "audio device still failing");
        }
        if !sleep_unless_stopped(stop, delay) {
            return;
        }
        delay = (delay * 2).min(backoff.max);
    }
}

fn sleep_unless_stopped(stop: &AtomicBool, total: Duration) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        thread::sleep(STOP_CHECK.min(deadline.saturating_duration_since(Instant::now())));
    }
    !stop.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: Backoff = Backoff {
        first: Duration::from_millis(1),
        max: Duration::from_millis(2),
        healthy_after: Duration::from_secs(60),
    };

    #[test]
    fn reopens_after_failures_until_the_stream_stops_cleanly() {
        let stop = AtomicBool::new(false);
        let mut attempts = 0;
        run_reopening("test", &stop, FAST, || {
            attempts += 1;
            if attempts < 3 {
                Err("device invalidated")
            } else {
                Ok(())
            }
        });
        assert_eq!(attempts, 3);
    }

    #[test]
    fn a_stop_request_ends_the_retries() {
        let stop = AtomicBool::new(false);
        let mut attempts = 0;
        run_reopening("test", &stop, FAST, || {
            attempts += 1;
            if attempts == 2 {
                stop.store(true, Ordering::SeqCst);
            }
            Err("device invalidated")
        });
        assert_eq!(attempts, 2);
    }
}
