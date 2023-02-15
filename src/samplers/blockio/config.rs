// Copyright 2019 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use serde_derive::Deserialize;
use strum::IntoEnumIterator;

use crate::config::SamplerConfig;

use super::stat::Statistic;
use super::sampler_config;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockIOConfig {
    #[serde(default)]
    bpf: bool,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    interval: Option<usize>,
    #[serde(default = "crate::common::default_percentiles")]
    percentiles: Vec<f64>,
    #[serde(default = "crate::common::default_distribution_percentiles")]
    distribution_percentiles: Vec<f64>,
    #[serde(default = "default_statistics")]
    statistics: Vec<Statistic>,
}

impl Default for BlockIOConfig {
    fn default() -> Self {
        Self {
            bpf: Default::default(),
            enabled: Default::default(),
            interval: Default::default(),
            percentiles: crate::common::default_percentiles(),
            distribution_percentiles: crate::common::default_distribution_percentiles(),
            statistics: default_statistics(),
        }
    }
}

fn default_statistics() -> Vec<Statistic> {
    Statistic::iter().collect()
}

sampler_config!(BlockIOConfig);
