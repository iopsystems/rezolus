use crate::*;

#[distributed_slice]
pub static CPU_CLASSIC_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];

#[distributed_slice]
pub static CPU_BPF_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];


#[distributed_slice(CLASSIC_SAMPLERS)]
fn cpu_classic(config: &Config) -> Box<dyn Sampler> {
    Box::new(Cpu::classic(config))
}

#[distributed_slice(BPF_SAMPLERS)]
fn cpu_bpf(config: &Config) -> Box<dyn Sampler> {
    Box::new(Cpu::bpf(config))
}


// #[cfg(feature = "bpf")]
// mod bpf;

mod classic;

mod stats;

pub struct Cpu {
    samplers: Vec<Box<dyn Sampler>>,
}

impl Cpu {
    fn classic(config: &Config) -> Self {
        let samplers = CPU_CLASSIC_SAMPLERS.iter().map(|init| init(config)).collect();
        Self {
            samplers,
        }
    }

    fn bpf(config: &Config) -> Self {
        let samplers = CPU_BPF_SAMPLERS.iter().map(|init| init(config)).collect();
        Self {
            samplers,
        }
    }
}

impl Display for Cpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "cpu")
    }
}

impl Sampler for Cpu {
    fn sample(&mut self) {
        for sampler in &mut self.samplers {
            sampler.sample()
        }
    }
}


