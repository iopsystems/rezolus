const NAME: &str = "memory_vmstat";

use crate::samplers::memory::linux::stats::*;
use crate::*;

use metriken::LazyCounter;
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
    counters: HashMap<&'static str, &'static LazyCounter>,
}

impl MeminfoInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let counters = HashMap::from([
            ("numa_hit", &MEMORY_NUMA_HIT),
            ("numa_miss", &MEMORY_NUMA_MISS),
            ("numa_foreign", &MEMORY_NUMA_FOREIGN),
            ("numa_interleave", &MEMORY_NUMA_INTERLEAVE),
            ("numa_local", &MEMORY_NUMA_LOCAL),
            ("numa_other", &MEMORY_NUMA_OTHER),
        ]);

        let file = std::fs::File::open("/proc/vmstat").map(File::from_std)?;

        Ok(Self {
            data: String::new(),
            file,
            counters,
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

            if let Some(counter) = self.counters.get_mut(*parts.first().unwrap()) {
                if let Some(Ok(v)) = parts.get(1).map(|v| v.parse::<u64>()) {
                    counter.set(v);
                }
            }
        }

        Ok(())
    }
}
