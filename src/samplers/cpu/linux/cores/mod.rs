const NAME: &str = "cpu_cores";

use crate::samplers::cpu::stats::*;
use crate::*;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let file = std::fs::File::open("/sys/devices/system/cpu/online")?;

    Ok(Some(Box::new(Cores {
        file: Mutex::new(File::from_std(file)),
    })))
}

pub struct Cores {
    file: Mutex<File>,
}

#[async_trait]
impl Sampler for Cores {
    async fn refresh(&self) {
        let mut file = self.file.lock().await;

        file.rewind().await.unwrap();

        let mut online = 0;

        let mut raw = String::new();

        let _ = file.read_to_string(&mut raw).await.unwrap();

        for range in raw.trim().split(',') {
            let mut parts = range.split('-');

            let first: Option<usize> = parts.next().map(|text| text.parse()).transpose().unwrap();
            let second: Option<usize> = parts.next().map(|text| text.parse()).transpose().unwrap();

            if parts.next().is_some() {
                // The line is invalid
                return;
            }

            match (first, second) {
                (Some(_), None) => {
                    online += 1;
                }
                (Some(start), Some(stop)) => {
                    online += stop + 1 - start;
                }
                _ => continue,
            }
        }

        let _ = CPU_CORES.set(online as _);
    }
}
