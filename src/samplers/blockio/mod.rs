use crate::*;

// #[distributed_slice]
// pub static SCHEDULER_CLASSIC_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];

#[distributed_slice]
pub static BLOCKIO_BPF_SAMPLERS: [fn(config: &Config) -> Box<dyn Sampler>] = [..];


// #[distributed_slice(CLASSIC_SAMPLERS)]
// fn scheduler_classic(config: &Config) -> Box<dyn Sampler> {
//     Box::new(Scheduler::classic(config))
// }

#[distributed_slice(BPF_SAMPLERS)]
fn scheduler_bpf(config: &Config) -> Box<dyn Sampler> {
    Box::new(BlockIO::bpf(config))
}


#[cfg(feature = "bpf")]
mod bpf;

// #[cfg(not(feature = "bpf"))]
// mod classic;

mod stats;

pub struct BlockIO {
    samplers: Vec<Box<dyn Sampler>>,
}

impl BlockIO {
    // fn classic(config: &Config) -> Self {
    //     let samplers = SCHEDULER_CLASSIC_SAMPLERS.iter().map(|init| init(config)).collect();
    //     Self {
    //         samplers,
    //     }
    // }

    fn bpf(config: &Config) -> Self {
        let samplers = BLOCKIO_BPF_SAMPLERS.iter().map(|init| init(config)).collect();
        Self {
            samplers,
        }
    }
}

impl Display for BlockIO {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "blockio")
    }
}

impl Sampler for BlockIO {
    fn sample(&mut self) {
        for sampler in &mut self.samplers {
            sampler.sample()
        }
    }
}

