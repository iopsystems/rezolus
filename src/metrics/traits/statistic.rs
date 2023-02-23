// Copyright 2020 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use crate::metrics::Source;

use core::hash::{Hash, Hasher};

/// A statistic represents a named entity that has associated measurements which
/// are recorded and metrics which are reported. This trait defines a set of
/// methods which uniquely identify the statistic, help the metrics library
/// track it appropriately, and allow including metadata in the exposition
/// format.
pub trait Statistic {
    /// The name is used to lookup the channel for the statistic and should be
    /// unique for each statistic. This field is used to hash the statistic in
    /// the core structure.
    fn name(&self) -> &str;
    /// Indicates which source type the statistic tracks.
    fn source(&self) -> Source;
    fn is_bpf(&self) -> bool;
}

impl Hash for dyn Statistic {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name().to_string().hash(state);
    }
}

impl PartialEq for dyn Statistic {
    fn eq(&self, other: &Self) -> bool {
        self.name() == other.name()
    }
}

impl Eq for dyn Statistic {}
