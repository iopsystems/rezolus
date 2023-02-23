// Copyright 2020 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use crate::metrics::StreamstatsError;
use heatmap::Error as HeatmapError;
use thiserror::Error;

/// Possible errors returned by operations on a histogram.
#[derive(Error, Debug, PartialEq)]
pub enum MetricsError {
    #[error("no samples for the statistic")]
    /// The summary contains no samples.
    Empty,
    #[error("invalid percentile")]
    /// The provided percentile is outside of the range 0.0 - 100.0 (inclusive)
    InvalidPercentile,
    #[error("statistic is not registered")]
    /// The statistic has not been registered
    NotRegistered,
    #[error("no summary configured for the statistic")]
    /// The statistic does not have a configured summary
    NoSummary,
    #[error("value out of range")]
    /// The requested value is out of range.
    OutOfRange,
    #[error("method does not apply for this statistic")]
    /// A method has been called which does not match the statistic source
    SourceMismatch,
}

impl From<heatmap::Error> for MetricsError {
    fn from(other: heatmap::Error) -> Self {
        match other {
            heatmap::Error::Empty => Self::Empty,
            heatmap::Error::InvalidPercentile => Self::InvalidPercentile,
            heatmap::Error::OutOfRange => Self::OutOfRange,
            heatmap::Error::InvalidConfig => unreachable!(),
        }
    }
}

#[derive(Error, Debug, PartialEq)]
pub enum SummaryError {
    #[error("summary contains no samples")]
    /// The summary contains no samples.
    Empty,
    #[error("invalid percentile")]
    /// The provided percentile is outside of the range 0.0 - 100.0 (inclusive)
    InvalidPercentile,
    #[error("no summary configured for the statistic")]
    /// There is no summary for the statistic
    NoSummary,
    #[error("value out of range")]
    /// The requested value is out of range.
    OutOfRange,
}

impl From<HeatmapError> for SummaryError {
    fn from(other: HeatmapError) -> Self {
        match other {
            HeatmapError::Empty => Self::Empty,
            HeatmapError::InvalidPercentile => Self::InvalidPercentile,
            HeatmapError::OutOfRange => Self::OutOfRange,
            HeatmapError::InvalidConfig => todo!(),
        }
    }
}

impl From<StreamstatsError> for SummaryError {
    fn from(other: StreamstatsError) -> Self {
        match other {
            StreamstatsError::Empty => Self::Empty,
            StreamstatsError::InvalidPercentile => Self::InvalidPercentile,
        }
    }
}
