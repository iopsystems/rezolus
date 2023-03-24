use crate::*;

static NAME: &str = "nop";

pub struct Nop {}

impl Nop {
    pub fn new(_config: &Config) -> Self {
        Self {}
    }
}

impl Sampler for Nop {
    fn sample(&mut self) {}
}
