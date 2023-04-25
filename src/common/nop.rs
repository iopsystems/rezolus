use crate::*;

/// A no-op sampler which can be used when a sampler may be conditionally
/// disabled.
pub struct Nop {}

impl Nop {
    pub fn new(_config: &Config) -> Self {
        Self {}
    }
}

impl Sampler for Nop {
    fn sample(&mut self) {}
}
