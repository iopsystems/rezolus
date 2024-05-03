use crate::Instant;
use core::time::Duration;

pub struct Interval {
    prev: Instant,
    next: Instant,
    period: Duration,
}

impl Interval {
    pub fn new(start: Instant, period: Duration) -> Self {
        Self {
            prev: start,
            next: start,
            period,
        }
    }

    /// Try to tick the interval forward to the provided instant. Returns true
    /// if the interval has fired and returns false otherwise.
    pub fn try_wait(&mut self, now: Instant) -> Result<Duration, ()> {
        if now < self.next {
            return Err(());
        }

        let next = self.next + self.period;

        // check if we have fallen behind
        if next > now {
            self.next = next;
        } else {
            // if we fell behind, don't sample again until the interval has
            // elapsed
            self.next = now + self.period;
        }

        let elapsed = now - self.prev;

        self.prev = now;

        Ok(elapsed)
    }
}
