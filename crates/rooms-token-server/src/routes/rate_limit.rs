//! Limitadores de taxa por janela deslizante para chat e login administrativo.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

const CHAT_RATE_LIMIT: usize = 10;
const CHAT_RATE_WINDOW: Duration = Duration::from_secs(10);
pub(super) const AVATAR_RATE_LIMIT: usize = 10;
const AVATAR_RATE_WINDOW: Duration = Duration::from_secs(60);
const ADMIN_LOGIN_RATE_LIMIT: usize = 5;
const ADMIN_LOGIN_WINDOW: Duration = Duration::from_secs(60);
const RATE_LIMIT_PRUNE_THRESHOLD: usize = 1024;

struct SlidingWindowLimiter {
    limit: usize,
    window: Duration,
    entries: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl SlidingWindowLimiter {
    fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock_entries(&self) -> MutexGuard<'_, HashMap<String, VecDeque<Instant>>> {
        match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn prune_map(&self, map: &mut HashMap<String, VecDeque<Instant>>, now: Instant) {
        if map.len() >= RATE_LIMIT_PRUNE_THRESHOLD {
            map.retain(|_, times| {
                prune_expired(times, now, self.window);
                !times.is_empty()
            });
        }
    }

    fn check_and_record_at(&self, key: &str, now: Instant) -> bool {
        let mut map = self.lock_entries();
        self.prune_map(&mut map, now);
        let times = map.entry(key.to_owned()).or_default();
        prune_expired(times, now, self.window);
        if times.len() >= self.limit {
            return false;
        }
        times.push_back(now);
        true
    }

    fn is_allowed_at(&self, keys: &[String], now: Instant) -> bool {
        let mut map = self.lock_entries();
        self.prune_map(&mut map, now);
        for key in keys {
            if let Some(times) = map.get_mut(key) {
                prune_expired(times, now, self.window);
                if times.len() >= self.limit {
                    return false;
                }
            }
        }
        true
    }

    fn record_at(&self, keys: &[String], now: Instant) {
        let mut map = self.lock_entries();
        self.prune_map(&mut map, now);
        for key in keys {
            let times = map.entry(key.clone()).or_default();
            prune_expired(times, now, self.window);
            times.push_back(now);
        }
    }

    fn clear(&self, key: &str) {
        let mut map = self.lock_entries();
        map.remove(key);
    }
}

pub struct ChatRateLimiter(SlidingWindowLimiter);

impl Default for ChatRateLimiter {
    fn default() -> Self {
        Self(SlidingWindowLimiter::new(CHAT_RATE_LIMIT, CHAT_RATE_WINDOW))
    }
}

impl ChatRateLimiter {
    pub fn check(&self, client_id: &str) -> bool {
        self.check_at(client_id, Instant::now())
    }

    fn check_at(&self, client_id: &str, now: Instant) -> bool {
        self.0.check_and_record_at(client_id, now)
    }
}

pub struct AvatarRateLimiter(SlidingWindowLimiter);

impl Default for AvatarRateLimiter {
    fn default() -> Self {
        Self(SlidingWindowLimiter::new(
            AVATAR_RATE_LIMIT,
            AVATAR_RATE_WINDOW,
        ))
    }
}

impl AvatarRateLimiter {
    pub fn check(&self, client_id: &str) -> bool {
        self.check_at(client_id, Instant::now())
    }

    fn check_at(&self, client_id: &str, now: Instant) -> bool {
        self.0.check_and_record_at(client_id, now)
    }
}

pub struct AdminLoginLimiter(SlidingWindowLimiter);

pub type RoomUnlockLimiter = AdminLoginLimiter;

impl Default for AdminLoginLimiter {
    fn default() -> Self {
        Self(SlidingWindowLimiter::new(
            ADMIN_LOGIN_RATE_LIMIT,
            ADMIN_LOGIN_WINDOW,
        ))
    }
}

impl AdminLoginLimiter {
    pub(super) fn is_allowed(&self, keys: &[String]) -> bool {
        self.is_allowed_at(keys, Instant::now())
    }

    fn is_allowed_at(&self, keys: &[String], now: Instant) -> bool {
        self.0.is_allowed_at(keys, now)
    }

    pub(super) fn record(&self, keys: &[String]) {
        self.record_at(keys, Instant::now());
    }

    fn record_at(&self, keys: &[String], now: Instant) {
        self.0.record_at(keys, now);
    }

    pub(super) fn clear(&self, key: &str) {
        self.0.clear(key);
    }
}

fn prune_expired(times: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while let Some(&front) = times.front() {
        if now.saturating_duration_since(front) >= window {
            times.pop_front();
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{AVATAR_RATE_LIMIT, AvatarRateLimiter, ChatRateLimiter};

    #[test]
    fn rate_limiter_allows_10_per_10s_and_resets_after_window() {
        let limiter = ChatRateLimiter::default();
        let t0 = Instant::now();
        for _ in 0..10 {
            assert!(limiter.check_at("client-a", t0));
        }
        assert!(!limiter.check_at("client-a", t0 + Duration::from_secs(9)));
        assert!(limiter.check_at("client-b", t0 + Duration::from_secs(9)));
        assert!(limiter.check_at("client-a", t0 + Duration::from_secs(10)));
    }

    #[test]
    fn avatar_rate_limiter_caps_uploads_per_minute() {
        let limiter = AvatarRateLimiter::default();
        let t0 = Instant::now();
        for _ in 0..AVATAR_RATE_LIMIT {
            assert!(limiter.check_at("client-a", t0));
        }
        assert!(!limiter.check_at("client-a", t0 + Duration::from_secs(59)));
        assert!(limiter.check_at("client-a", t0 + Duration::from_secs(60)));
    }
}
