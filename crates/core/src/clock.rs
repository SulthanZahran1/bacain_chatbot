//! Injectable clock — deterministic freshness windows and cooldowns in tests.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type Clock = Arc<dyn Now>;

pub trait Now: Send + Sync {
    fn now_unix(&self) -> i64;
}

#[derive(Default)]
pub struct SystemClock;

impl Now for SystemClock {
    fn now_unix(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs() as i64
    }
}

/// Test double: a clock whose time is a manually-controlled unix timestamp.
pub struct FakeClock {
    t: AtomicI64,
}

impl FakeClock {
    pub fn new(start_unix: i64) -> Self {
        Self {
            t: AtomicI64::new(start_unix),
        }
    }
    pub fn advance(&self, secs: i64) {
        self.t.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Now for FakeClock {
    fn now_unix(&self) -> i64 {
        self.t.load(Ordering::SeqCst)
    }
}

pub fn system() -> Clock {
    Arc::new(SystemClock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_advances() {
        let c = FakeClock::new(1000);
        assert_eq!(c.now_unix(), 1000);
        c.advance(60);
        assert_eq!(c.now_unix(), 1060);
    }
}
