use crate::*;

/// A no-op sampler which can be used when a sampler may be conditionally
/// disabled.
pub struct Nop {}

// Since the no-op sampler is not always used in a build (depending on platform
// and features), we suppress the dead code lints.
#[allow(dead_code)]
impl Nop {
    pub fn new(_config: &Config) -> Self {
        Self {}
    }
}

impl Sampler for Nop {
    fn sample(&mut self) {}
}
