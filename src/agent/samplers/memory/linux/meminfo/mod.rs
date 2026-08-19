const NAME: &str = "memory_meminfo";

use crate::agent::*;

use metriken::LazyGauge;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

use std::collections::HashMap;

mod stats;

use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = MeminfoInner::new()?;

    Ok(Some(Box::new(Meminfo {
        inner: inner.into(),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Meminfo {
    inner: Mutex<MeminfoInner>,
}

#[async_trait]
impl Sampler for Meminfo {
    fn name(&self) -> &'static str {
        NAME
    }

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

    // Acquisition-group bracket (principle 18): `MEMINFO_ACQ.acquire()` before
    // the read, `guard.finish()` after every value from this parse is set.
    // Any read error (`?`) drops the guard without `finish()` — the previous
    // window stands (discard-on-error; missing beats wrong). A partial parse
    // (some recognized keys found, others missing from this particular
    // `/proc/meminfo` snapshot) is NOT an error path: the loop only ever sets
    // the keys it actually finds, so there is no "some values set, then an
    // error" case here to decide between finish/discard — the read itself is
    // the only failure mode, and it fails before any `set()` call.
    pub async fn refresh(&mut self) -> Result<(), std::io::Error> {
        let guard = MEMINFO_ACQ.acquire();

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

        guard.finish();

        Ok(())
    }
}
