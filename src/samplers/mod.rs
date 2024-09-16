use async_trait::async_trait;

mod blockio;
mod cpu;
mod gpu;
mod memory;
mod network;
mod rezolus;
mod scheduler;
mod syscall;
mod tcp;

#[async_trait]
pub trait Sampler: Send + Sync {
    async fn refresh(&self);
}

pub type SamplerResult = anyhow::Result<Option<Box<dyn Sampler>>>;
