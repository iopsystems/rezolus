use crate::agent::Config;
use crate::debug;
use crate::Instant;
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
    fn name(&self) -> &'static str;

    async fn refresh(&self);

    async fn refresh_with_logging(&self) {
        let start = Instant::now();

        self.refresh().await;

        let duration = start.elapsed().as_micros();

        debug!("{} sampling latency: {duration} us", self.name());
    }
}

pub type SamplerResult = anyhow::Result<Option<Box<dyn Sampler>>>;
