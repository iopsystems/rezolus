const NAME: &str = "tcp_connection_state";

use crate::samplers::tcp::linux::stats::*;
use crate::*;

use metriken::LazyGauge;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = ConnectionStateInner::new()?;

    Ok(Some(Box::new(ConnectionState {
        inner: Arc::new(Mutex::new(inner)),
    })))
}

pub struct ConnectionState {
    inner: Arc<Mutex<ConnectionStateInner>>,
}

#[async_trait]
impl Sampler for ConnectionState {
    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

pub struct ConnectionStateInner {
    files: Vec<File>,
    gauges: Vec<(&'static LazyGauge, i64)>,
}

impl ConnectionStateInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let gauges: Vec<(&'static LazyGauge, i64)> = vec![
            (&TCP_CONN_STATE_ESTABLISHED, 0),
            (&TCP_CONN_STATE_SYN_SENT, 0),
            (&TCP_CONN_STATE_SYN_RECV, 0),
            (&TCP_CONN_STATE_FIN_WAIT1, 0),
            (&TCP_CONN_STATE_FIN_WAIT2, 0),
            (&TCP_CONN_STATE_TIME_WAIT, 0),
            (&TCP_CONN_STATE_CLOSE, 0),
            (&TCP_CONN_STATE_CLOSE_WAIT, 0),
            (&TCP_CONN_STATE_LAST_ACK, 0),
            (&TCP_CONN_STATE_LISTEN, 0),
            (&TCP_CONN_STATE_CLOSING, 0),
            (&TCP_CONN_STATE_NEW_SYN_RECV, 0),
        ];

        let ipv4 = std::fs::File::open("/proc/net/tcp").map_err(|e| {
            error!("Failed to open /proc/net/tcp: {e}");
        });

        let ipv6 = std::fs::File::open("/proc/net/tcp6").map_err(|e| {
            error!("Failed to open /proc/net/tcp6: {e}");
        });

        let mut files: Vec<Result<std::fs::File, ()>> = vec![ipv4, ipv6];

        let files: Vec<File> = files
            .drain(..)
            .filter_map(|v| v.ok())
            .map(File::from_std)
            .collect();

        if files.is_empty() {
            return Err(std::io::Error::other(
                "Could not open any file in /proc/net for this sampler",
            ));
        }

        Ok(Self { files, gauges })
    }

    async fn refresh(&mut self) {
        // zero the temporary gauges
        for (_, gauge) in self.gauges.iter_mut() {
            *gauge = 0;
        }

        for file in self.files.iter_mut() {
            // seek to start to cause reload of content
            if file.rewind().await.is_ok() {
                let mut data = String::new();
                if file.read_to_string(&mut data).await.is_err() {
                    error!("error reading /proc/net/tcp");
                    return;
                }

                for line in data.lines() {
                    let parts: Vec<&str> = line.split_whitespace().collect();

                    // find and increment the temporary gauge for this state
                    if let Some(Ok(state)) = parts.get(3).map(|v| u8::from_str_radix(v, 16)) {
                        if let Some((_, gauge)) = self.gauges.get_mut(state as usize - 1) {
                            *gauge += 1;
                        }
                    }
                }
            }
        }

        for (gauge, value) in self.gauges.iter() {
            gauge.set(*value);
        }
    }
}
