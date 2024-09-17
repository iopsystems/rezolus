const NAME: &str = "memory_meminfo";

use crate::common::*;
use crate::samplers::memory::linux::stats::*;
use crate::*;

use metriken::LazyGauge;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

use std::collections::HashMap;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = MeminfoInner::new()?;

    Ok(Some(Box::new(Meminfo {
        inner: inner.into(),
    })))
}

struct Meminfo {
    inner: Mutex<MeminfoInner>,
}

#[async_trait]
impl Sampler for Meminfo {
    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

struct MeminfoInner {
    data: String,
    file: File,
    gauges: HashMap<&'static str, &'static LazyGauge>,
}

impl MeminfoInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let gauges = HashMap::from([
            ("MemTotal:", &MEMORY_TOTAL),
            ("MemFree:", &MEMORY_FREE),
            ("MemAvailable:", &MEMORY_AVAILABLE),
            ("Buffers:", &MEMORY_BUFFERS),
            ("Cached:", &MEMORY_CACHED),
        ]);

        let file = std::fs::File::open("/proc/meminfo").map(File::from_std)?;

        Ok(Self {
            data: String::new(),
            file,
            gauges,
        })
    }

    pub async fn refresh(&mut self) -> Result<(), std::io::Error> {
        self.file.rewind().await?;

        self.data.clear();

        self.file.read_to_string(&mut self.data).await?;

        let lines = self.data.lines();

        for line in lines {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.is_empty() {
                continue;
            }

            if let Some(gauge) = self.gauges.get_mut(*parts.first().unwrap()) {
                if let Some(Ok(v)) = parts.get(1).map(|v| v.parse::<i64>()) {
                    gauge.set(v * KIBIBYTES as i64);
                }
            }
        }

        Ok(())
    }
}
