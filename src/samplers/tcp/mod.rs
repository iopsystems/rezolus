use crate::*;

#[distributed_slice]
pub static TCP_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];


#[distributed_slice(SAMPLERS)]
fn tcp(config: &Config) -> Box<dyn Sampler> {
    Box::new(Tcp::new(config))
}

#[cfg(feature = "bpf")]
mod bpf;

#[cfg(not(feature = "bpf"))]
mod classic;

mod stats;

pub struct Tcp {
    samplers: Vec<Box<dyn Sampler>>,
}

impl Tcp {
    fn new(config: &Config) -> Self {
        let samplers = TCP_SAMPLERS.iter().map(|init| init(config)).collect();
        Self {
            samplers,
        }
    }
}

impl Display for Tcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "tcp")
    }
}

impl Sampler for Tcp {
    fn sample(&mut self) {
        for sampler in &mut self.samplers {
            sampler.sample()
        }
    }
}


