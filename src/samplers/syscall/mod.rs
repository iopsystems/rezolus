use crate::*;

// #[distributed_slice]
// pub static SCHEDULER_CLASSIC_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];

#[distributed_slice]
pub static SYSCALL_BPF_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];


// #[distributed_slice(CLASSIC_SAMPLERS)]
// fn scheduler_classic(config: &Config) -> Box<dyn Sampler> {
//     Box::new(Scheduler::classic(config))
// }

#[distributed_slice(BPF_SAMPLERS)]
fn syscall_bpf(config: &Config) -> Box<dyn Sampler> {
    Box::new(Syscall::bpf(config))
}


#[cfg(feature = "bpf")]
mod bpf;

// #[cfg(not(feature = "bpf"))]
// mod classic;

mod stats;

pub struct Syscall {
    samplers: Vec<Box<dyn Sampler>>,
}

impl Syscall {
    // fn classic(config: &Config) -> Self {
    //     let samplers = SCHEDULER_CLASSIC_SAMPLERS.iter().map(|init| init(config)).collect();
    //     Self {
    //         samplers,
    //     }
    // }

    fn bpf(config: &Config) -> Self {
        let samplers = SYSCALL_BPF_SAMPLERS.iter().map(|init| init(config)).collect();
        Self {
            samplers,
        }
    }
}

impl Display for Syscall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "syscall")
    }
}

impl Sampler for Syscall {
    fn sample(&mut self) {
        for sampler in &mut self.samplers {
            sampler.sample()
        }
    }
}


