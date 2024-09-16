const NAME: &str = "filesystem_descriptors";

use crate::common::*;
use crate::samplers::filesystem::linux::stats::*;
use crate::*;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = DescriptorsInner::new()?;

    Ok(Some(Box::new(Descriptors {
        inner: Arc::new(Mutex::new(inner)),
    })))
}

pub struct Descriptors {
    inner: Arc<Mutex<DescriptorsInner>>,
}

#[async_trait]
impl Sampler for Descriptors {
    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

pub struct DescriptorsInner {
    file: File,
}

impl DescriptorsInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let file = std::fs::File::open("/proc/sys/fs/file-nr")
            .map(File::from_std)?;


        Ok(Self {
            file,
        })
    }

    async fn refresh(&mut self) {
        self.file.rewind().await?;

        let mut data = String::new();
        self.file.read_to_string(&mut data).await?;

        let mut lines = data.lines();

        if let Some(line) = lines.next() {
            let parts: Vec<&str> = line.split_whitespace().collect();

            if parts.len() == 3 {
                if let Ok(open) = parts[0].parse::<i64>() {
                    FILESYSTEM_DESCRIPTORS_OPEN.set(open);
                }
            }
        }

        Ok(())
    }
}
