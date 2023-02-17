// Copyright 2020 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use core::hash::Hash;
use core::hash::Hasher;

use crate::metrics::*;

pub struct Entry {
    name: String,
    source: Source,
    is_bpf: bool,
}

impl Clone for Entry {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            source: self.source,
            is_bpf: self.is_bpf,
        }
    }
}

impl Statistic for Entry {
    fn name(&self) -> &str {
        &self.name
    }

    fn source(&self) -> Source {
        self.source
    }

    fn is_bpf(&self) -> bool {
        self.is_bpf
    }
}

impl Hash for Entry {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl From<&dyn Statistic> for Entry {
    fn from(statistic: &dyn Statistic) -> Self {
        Self {
            name: statistic.name().to_string(),
            source: statistic.source(),
            is_bpf: statistic.is_bpf(),
        }
    }
}
impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Entry {}
