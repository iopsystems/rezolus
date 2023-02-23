// Copyright 2020 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use heatmap::Heatmap;
use crate::metrics::entry::Entry;
use crate::metrics::outputs::ApproxOutput;
use crate::metrics::traits::*;
use crate::metrics::MetricsError;
use crate::metrics::Output;
use crate::metrics::*;
use clocksource::*;

use crossbeam::atomic::AtomicCell;
use dashmap::DashSet;

type Duration = clocksource::Duration<clocksource::Nanoseconds<u64>>;

// use rustcommon_atomics::{Atomic, AtomicBool, Ordering};

/// Internal type which stores fields necessary to track a corresponding
/// statistic.
pub struct Channel {
    refreshed: AtomicCell<Instant<Nanoseconds<u64>>>,
    statistic: Entry,
    empty: AtomicBool,
    reading: AtomicU64,
    heatmap: Heatmap,
    outputs: DashSet<ApproxOutput>,
}

impl Channel {
    /// Creates an empty channel for a statistic.
    pub fn new(statistic: &dyn Statistic) -> Self {
        Self {
            empty: AtomicBool::new(true),
            statistic: Entry::from(statistic),
            reading: Default::default(),
            refreshed: AtomicCell::new(Instant::<Nanoseconds<u64>>::now()),
            heatmap: Heatmap::new(0, 4, 64, Duration::from_secs(60), Duration::from_secs(1)).expect("failed to create heatmap"),
            outputs: Default::default(),
        }
    }

    /// Records a bucket value + count pair into the summary.
    pub fn record_bucket(
        &self,
        time: Instant<Nanoseconds<u64>>,
        value: u64,
        count: u32,
    ) {
        self.heatmap.increment(time, value, count);
    }

    /// Updates a counter to a new value if the reading is newer than the stored
    /// reading.
    pub fn record_counter(&self, time: Instant<Nanoseconds<u64>>, value: u64) {
        let t0 = self.refreshed.load();
        if time <= t0 {
            return;
        }
        if !self.empty.load(Ordering::Relaxed) {
            self.refreshed.store(time);
            let v0 = self.reading.load(Ordering::Relaxed);
            let dt = time - t0;
            let dv = (value - v0) as f64;
            let rate = (dv
                / (dt.as_secs() as f64 + dt.subsec_nanos() as f64 / 1_000_000_000.0))
                .ceil();
            self.heatmap.increment(time, rate as u64, 1);
            self.reading.store(value, Ordering::Relaxed);
        } else {
            self.reading.store(value, Ordering::Relaxed);
            self.empty.store(false, Ordering::Relaxed);
            self.refreshed.store(time);
        }
    }

    /// Increment a counter by an amount
    pub fn increment_counter(&self, value: u64) {
        self.empty.store(false, Ordering::Relaxed);
        self.reading.fetch_add(value, Ordering::Relaxed);
    }

    /// Updates a gauge reading if the new value is newer than the stored value.
    pub fn record_gauge(&self, time: Instant<Nanoseconds<u64>>, value: u64) {
        {
            let t0 = self.refreshed.load();
            if time <= t0 {
                return;
            }
        }
        self.heatmap.increment(time, value, 1_u8.into());
        self.reading.store(value, Ordering::Relaxed);
        self.empty.store(false, Ordering::Relaxed);
        self.refreshed.store(time);
    }

    /// Returns a percentile across stored readings/rates/...
    pub fn percentile(&self, percentile: f64) -> Result<u64, MetricsError> {
        self.heatmap.percentile(percentile).map(|v| v.high()).map_err(MetricsError::from)
    }

    /// Returns the main reading for the channel (eg: counter, gauge)
    pub fn reading(&self) -> Result<u64, MetricsError> {
        if !self.empty.load(Ordering::Relaxed) {
            Ok(self.reading.load(Ordering::Relaxed))
        } else {
            Err(MetricsError::Empty)
        }
    }

    pub fn statistic(&self) -> &dyn Statistic {
        &self.statistic
    }

    pub fn outputs(&self) -> Vec<ApproxOutput> {
        let mut ret = Vec::new();
        for output in self.outputs.iter().map(|v| *v) {
            ret.push(output);
        }
        ret
    }

    pub fn add_output(&self, output: Output) {
        self.outputs.insert(ApproxOutput::from(output));
    }

    pub fn remove_output(&self, output: Output) {
        self.outputs.remove(&ApproxOutput::from(output));
    }
}
