use crate::*;

#[distributed_slice]
pub static TCP_CLASSIC_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];

#[distributed_slice]
pub static TCP_BPF_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];


#[distributed_slice(CLASSIC_SAMPLERS)]
fn tcp_classic(config: &Config) -> Box<dyn Sampler> {
    Box::new(Tcp::classic(config))
}

#[distributed_slice(BPF_SAMPLERS)]
fn tcp_bpf(config: &Config) -> Box<dyn Sampler> {
    Box::new(Tcp::bpf(config))
}


#[cfg(feature = "bpf")]
mod bpf;

mod classic;
mod stats;

pub struct Tcp {
    samplers: Vec<Box<dyn Sampler>>,
}

impl Tcp {
    fn classic(config: &Config) -> Self {
        let samplers = TCP_CLASSIC_SAMPLERS.iter().map(|init| init(config)).collect();
        Self {
            samplers,
        }
    }

    fn bpf(config: &Config) -> Self {
        let samplers = TCP_BPF_SAMPLERS.iter().map(|init| init(config)).collect();
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


