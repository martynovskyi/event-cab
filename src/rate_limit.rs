//! In-memory fixed-window rate limiter for ingest (§7 step 3).

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Hash, PartialEq, Eq)]
enum Key {
    Client(String),
    Topic(String, String),
}

struct Window {
    start: Instant,
    count: u32,
}

pub struct RateLimiter {
    buckets: Mutex<HashMap<Key, Window>>,
    period: Duration,
}

impl RateLimiter {
    pub fn new(period: Duration) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            period,
        }
    }

    pub fn per_minute() -> Self {
        Self::new(Duration::from_secs(60))
    }

    /// Checks the global per-client bucket first; the (topic, client) bucket is only created
    /// once that passes, so rotating topic names can neither bypass the limit nor grow the map.
    pub fn check_ingest(&self, client: &str, topic: &str, per_client: u32, per_topic: u32) -> bool {
        let now = Instant::now();
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        hit(
            &mut b,
            Key::Client(client.to_owned()),
            per_client,
            now,
            self.period,
        ) && hit(
            &mut b,
            Key::Topic(topic.to_owned(), client.to_owned()),
            per_topic,
            now,
            self.period,
        )
    }

    /// Drops expired windows.
    pub fn prune(&self) {
        let now = Instant::now();
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        b.retain(|_, w| now.duration_since(w.start) < self.period);
    }

    pub fn bucket_count(&self) -> usize {
        self.buckets.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

fn hit(b: &mut HashMap<Key, Window>, key: Key, limit: u32, now: Instant, period: Duration) -> bool {
    let w = b.entry(key).or_insert(Window {
        start: now,
        count: 0,
    });
    if now.duration_since(w.start) >= period {
        *w = Window {
            start: now,
            count: 0,
        };
    }
    if w.count >= limit {
        return false;
    }
    w.count += 1;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_topic_limit() {
        let rl = RateLimiter::per_minute();
        assert!(rl.check_ingest("c", "t", 10, 2));
        assert!(rl.check_ingest("c", "t", 10, 2));
        assert!(!rl.check_ingest("c", "t", 10, 2));
        assert!(rl.check_ingest("c", "other", 10, 2));
        assert!(rl.check_ingest("c2", "t", 10, 2));
    }

    #[test]
    fn rotating_topics_hits_global_limit_without_growing_map() {
        let rl = RateLimiter::per_minute();
        for i in 0..3 {
            assert!(rl.check_ingest("c", &format!("t{i}"), 3, 20));
        }
        for i in 3..100 {
            assert!(!rl.check_ingest("c", &format!("t{i}"), 3, 20));
        }
        // 1 client bucket + 3 topic buckets
        assert_eq!(rl.bucket_count(), 4);
    }

    #[test]
    fn window_resets_and_prune() {
        let rl = RateLimiter::new(Duration::from_millis(20));
        assert!(rl.check_ingest("c", "t", 1, 1));
        assert!(!rl.check_ingest("c", "t", 1, 1));
        std::thread::sleep(Duration::from_millis(30));
        rl.prune();
        assert_eq!(rl.bucket_count(), 0);
        assert!(rl.check_ingest("c", "t", 1, 1));
    }
}
