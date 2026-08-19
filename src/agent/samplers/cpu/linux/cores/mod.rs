const NAME: &str = "cpu_cores";

use crate::agent::*;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

mod stats;

use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let file = std::fs::File::open("/sys/devices/system/cpu/online")?;

    Ok(Some(Box::new(Cores {
        file: Mutex::new(File::from_std(file)),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

pub struct Cores {
    file: Mutex<File>,
}

#[async_trait]
impl Sampler for Cores {
    fn name(&self) -> &'static str {
        NAME
    }

    // Acquisition-group bracket (principle 18): acquire before the read,
    // finish after the single `set()`. An invalid `online` line drops the
    // guard without `finish()` (discard-on-error, same as a read failure) —
    // the sampler never partially-computed `online` before this check, so
    // there is no "some values set" case to distinguish here.
    async fn refresh(&self) {
        let mut file = self.file.lock().await;

        let guard = CPU_CORES_ACQ.acquire();

        file.rewind().await.unwrap();

        let mut online = 0;

        let mut raw = String::new();

        let _ = file.read_to_string(&mut raw).await.unwrap();

        for range in raw.trim().split(',') {
            let mut parts = range.split('-');

            let first: Option<usize> = parts.next().map(|text| text.parse()).transpose().unwrap();
            let second: Option<usize> = parts.next().map(|text| text.parse()).transpose().unwrap();

            if parts.next().is_some() {
                // The line is invalid: discard (drop the guard) rather than
                // finish with a value computed from a partial parse.
                return;
            }

            match (first, second) {
                (Some(_single), None) => {
                    online += 1;
                }
                (Some(start), Some(stop)) => {
                    online += stop + 1 - start;
                }
                _ => continue,
            }
        }

        CPU_CORES.set(online as _);
        guard.finish();
    }
}
