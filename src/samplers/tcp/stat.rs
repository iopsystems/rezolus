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
    #[strum(serialize = "tcp/jitter")]
    Jitter,
    #[strum(serialize = "tcp/transmit/retransmit_timeout")]
    RetransmissionTimeout,
    #[strum(serialize = "tcp/srtt")]
    SmoothedRoundTripTime,
    #[strum(serialize = "tcp/rx/size")]
    RxSize,
    #[strum(serialize = "tcp/rx/bytes")]
    RxBytes,
    #[strum(serialize = "tcp/rx/packets")]
    RxPackets,
    #[strum(serialize = "tcp/tx/size")]
    TxSize,
    #[strum(serialize = "tcp/tx/bytes")]
    TxBytes,
    #[strum(serialize = "tcp/tx/packets")]
    TxPackets,
}

impl crate::Statistic for Statistic {
    fn name(&self) -> &str {
        (*self).into()
    }

    fn source(&self) -> Source {
        match *self {
            Self::Jitter | Self::SmoothedRoundTripTime | Self::RxSize | Self::TxSize => Source::Distribution,
            _ => Source::Counter,
        }
    }

    fn is_bpf(&self) -> bool {
        true
    }
}
