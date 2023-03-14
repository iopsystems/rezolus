use crate::*;

#[distributed_slice]
pub static MEMORY_CLASSIC_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];

#[distributed_slice]
pub static MEMORY_BPF_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];


#[distributed_slice(CLASSIC_SAMPLERS)]
fn cpu_classic(config: &Config) -> Box<dyn Sampler> {
    Box::new(Memory::classic(config))
}

#[distributed_slice(BPF_SAMPLERS)]
fn cpu_bpf(config: &Config) -> Box<dyn Sampler> {
    Box::new(Memory::bpf(config))
}


// #[cfg(feature = "bpf")]
// mod bpf;

mod classic;

mod stats;

pub struct Memory {
    samplers: Vec<Box<dyn Sampler>>,
}

impl Memory {
    fn classic(config: &Config) -> Self {
        let samplers = MEMORY_CLASSIC_SAMPLERS.iter().map(|init| init(config)).collect();
        Self {
            samplers,
        }
    }

    fn bpf(config: &Config) -> Self {
        let samplers = MEMORY_BPF_SAMPLERS.iter().map(|init| init(config)).collect();
        Self {
            samplers,
        }
    }
}

impl Display for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "memory")
    }
}

impl Sampler for Memory {
    fn sample(&mut self) {
        for sampler in &mut self.samplers {
            sampler.sample()
        }
    }
}


