use crate::Instant;
use core::time::Duration;

pub struct AsyncInterval {
    inner: tokio::time::Interval,
    last: Option<tokio::time::Instant>,
}

impl AsyncInterval {
    pub fn new(period: Duration) -> Self {
        let mut inner = tokio::time::interval(period);
        inner.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        Self { inner, last: None }
    }

    pub async fn tick(&mut self) -> (tokio::time::Instant, Option<Duration>) {
        let now = self.inner.tick().await;

        let elapsed = self.last.map(|v| now.duration_since(v));
        self.last = Some(now);

        (now, elapsed)
    }
}

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
