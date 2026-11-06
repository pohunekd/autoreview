//! Test doubles of the engine's time source.

// Rust guideline compliant 2026-06-26

use std::future::{Future, ready};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use time::macros::datetime;

use crate::engine::Clock;

/// A clock that moves only when a wait ends, so retry tests never sleep.
pub(crate) struct FakeClock {
    /// The instant the clock starts from; only offsets from it are observable.
    start: Instant,
    /// Time that the waits have advanced the clock by.
    elapsed: Mutex<Duration>,
}

impl FakeClock {
    /// Creates a clock at its start, with no time elapsed.
    pub(crate) fn new() -> Self {
        Self {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        }
    }

    /// Returns the time that the waits advanced the clock by.
    pub(crate) fn elapsed(&self) -> Duration {
        *self.elapsed.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.start
            .checked_add(self.elapsed())
            .expect("the fake clock stays within the Instant range")
    }

    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send {
        let mut elapsed = self.elapsed.lock().unwrap_or_else(PoisonError::into_inner);
        *elapsed = elapsed.saturating_add(delay);
        ready(())
    }

    fn wall_now(&self) -> OffsetDateTime {
        let elapsed = time::Duration::try_from(self.elapsed())
            .expect("the fake clock stays within the time::Duration range");
        datetime!(2026-01-01 0:00 UTC) + elapsed
    }
}
