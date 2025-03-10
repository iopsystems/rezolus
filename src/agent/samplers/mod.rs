use crate::agent::Config;
use async_trait::async_trait;
use linkme::distributed_slice;
use std::sync::Arc;

mod blockio;
mod cpu;
mod gpu;
mod memory;
mod network;
mod rezolus;
mod scheduler;
mod syscall;
mod tcp;

#[distributed_slice]
pub static SAMPLERS: [fn(config: Arc<Config>) -> SamplerResult] = [..];

#[async_trait]
pub trait Sampler: Send + Sync {
    async fn refresh(&self);
}

pub type SamplerResult = anyhow::Result<Option<Box<dyn Sampler>>>;
