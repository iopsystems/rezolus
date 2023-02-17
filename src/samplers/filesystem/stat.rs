// Copyright 2023 IOP Systems, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use crate::metrics::*;
use serde_derive::{Deserialize, Serialize};
use strum_macros::{EnumIter, EnumString, IntoStaticStr};

#[cfg(feature = "bpf")]
use crate::common::bpf::*;

#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    EnumIter,
    EnumString,
    Eq,
    IntoStaticStr,
    PartialEq,
    Hash,
    Serialize,
)]
#[serde(deny_unknown_fields, try_from = "&str", into = "&str")]
#[allow(clippy::enum_variant_names)]
pub enum Statistic {
    #[strum(serialize = "filesystem/read/latency")]
    ReadLatency,
    #[strum(serialize = "filesystem/write/latency")]
    WriteLatency,
    #[strum(serialize = "filesystem/open/latency")]
    OpenLatency,
    #[strum(serialize = "filesystem/fsync/latency")]
    FsyncLatency,
}

impl crate::Statistic for Statistic {
    fn name(&self) -> &str {
        (*self).into()
    }

    fn source(&self) -> Source {
        Source::Distribution
    }

    fn is_bpf(&self) -> bool {
        true
    }
}
