// Copyright 2020 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use super::*;

use std::sync::RwLock;
use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum StreamstatsError {
    #[error("streamstats contains no samples")]
    /// There are no samples in the streamstats struct
    Empty,
    #[error("invalid percentile")]
    /// The requested percentile is not in the range 0.0 - 100.0
    InvalidPercentile,
}

/// A datastructure for concurrently writing a stream of values into a buffer
/// which can be used to produce summary statistics such as percentiles.
pub struct Streamstats {
    buffer: Vec<AtomicU64>,
    current: AtomicUsize,
    len: AtomicUsize,
    sorted: RwLock<Vec<u64>>,
}

impl Streamstats {
    /// Create a new struct which can hold up to `capacity` values in the
    /// buffer.
    pub fn new(capacity: usize) -> Self {
        let mut buffer = Vec::with_capacity(capacity);
        let sorted = RwLock::new(Vec::<u64>::with_capacity(capacity));
        for _ in 0..capacity {
            buffer.push(Default::default());
        }
        Self {
            buffer,
            current: AtomicUsize::new(0),
            len: AtomicUsize::new(0),
            sorted,
        }
    }

    /// Insert a new value into the buffer.
    pub fn insert(&self, value: u64) {
        let mut current = self.current.load(Ordering::Relaxed);
        self.buffer[current].store(value, Ordering::Relaxed);
        loop {
            let next = if current < (self.buffer.len() - 1) {
                current + 1
            } else {
                0
            };
            let result =
                self.current
                    .compare_exchange(current, next, Ordering::Relaxed, Ordering::Relaxed);
            match result {
                Ok(_) => {
                    break;
                }
                Err(v) => {
                    current = v;
                }
            }
        }
        if self.len.load(Ordering::Relaxed) < self.buffer.len() {
            self.len.fetch_add(1, Ordering::Relaxed);
        }
        self.sorted.write().unwrap().clear(); // resort required
    }

    fn values(&self) -> usize {
        let len = self.len.load(Ordering::Relaxed);
        if len < self.buffer.len() {
            len
        } else {
            self.buffer.len()
        }
    }

    /// Return the value closest to the specified percentile. Returns an error
    /// if the value is outside of the histogram range or if the histogram is
    /// empty. Percentile must be within the range 0.0 to 100.0
    pub fn percentile(
        &self,
        percentile: f64,
    ) -> Result<u64, StreamstatsError> {
        if !(0.0..=100.0).contains(&percentile) {
            return Err(StreamstatsError::InvalidPercentile);
        }
        let sorted_len = { self.sorted.read().unwrap().len() };
        if sorted_len == 0 {
            let values = self.values();
            if values == 0 {
                return Err(StreamstatsError::Empty);
            } else {
                let mut sorted = self.sorted.write().unwrap();
                let values = self.values();
                for i in 0..values {
                    sorted.push(self.buffer[i].load(Ordering::Relaxed));
                }
                sorted.sort();
            }
        }
        let sorted = self.sorted.read().unwrap();
        if sorted.len() > 0 {
            if percentile == 0.0 {
                Ok(sorted[0])
            } else {
                let need = (percentile / 100.0 * sorted.len() as f64).ceil() as usize;
                Ok(sorted[need - 1])
            }
        } else {
            Err(StreamstatsError::Empty)
        }
    }

    /// Clear all samples from the buffer.
    pub fn clear(&mut self) {
        self.current.store(0, Ordering::Relaxed);
        self.len.store(0, Ordering::Relaxed);
        self.sorted.write().unwrap().clear();
    }
}