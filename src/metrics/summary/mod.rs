// Copyright 2020 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use crate::metrics::error::SummaryError;
use crate::metrics::*;
use heatmap::Heatmap;

// use rustcommon_heatmap::{AtomicHeatmap, Duration, Instant};
use crate::metrics::Streamstats;

pub(crate) enum SummaryStruct {
    Heatmap(Heatmap),
    Stream(Streamstats),
}

impl SummaryStruct {
    pub fn increment(&self, time: Instant<Nanoseconds<u64>>, value: u64, count: u32) {
        match self {
            Self::Heatmap(heatmap) => heatmap.increment(time, value, count),
            Self::Stream(stream) => stream.insert(value),
        }
    }

    pub fn percentile(&self, percentile: f64) -> Result<u64, SummaryError> {
        match self {
            Self::Heatmap(heatmap) => heatmap
                .percentile(percentile)
                .map_err(SummaryError::from)
                .map(|b| b.high()),
            Self::Stream(stream) => stream.percentile(percentile).map_err(SummaryError::from),
        }
    }

    pub fn heatmap(
        span: Duration<Nanoseconds<u64>>,
        resolution: Duration<Nanoseconds<u64>>,
    ) -> Self {
        Self::Heatmap(Heatmap::new(0, 4, 64, span, resolution).expect("failed to create heatmap"))
    }

    pub fn stream(samples: usize) -> Self {
        Self::Stream(Streamstats::new(samples))
    }
}

enum SummaryType {
    Heatmap(
        Duration<Nanoseconds<u64>>,
        Duration<Nanoseconds<u64>>,
    ),
    Stream(usize),
}

pub struct Summary {
    inner: SummaryType,
}

impl Summary {
    pub fn heatmap(
        span: Duration<Nanoseconds<u64>>,
        resolution: Duration<Nanoseconds<u64>>,
    ) -> Summary {
        Self {
            inner: SummaryType::Heatmap(span, resolution),
        }
    }

    pub fn stream(samples: usize) -> Summary {
        Self {
            inner: SummaryType::Stream(samples),
        }
    }

    pub(crate) fn build(&self) -> SummaryStruct {
        match self.inner {
            SummaryType::Heatmap(span, resolution) => {
                SummaryStruct::heatmap(span, resolution)
            }
            SummaryType::Stream(samples) => SummaryStruct::stream(samples),
        }
    }
}
