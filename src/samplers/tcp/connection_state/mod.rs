use crate::common::Nop;
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;
use metriken::Gauge;
use std::fs::File;
use std::io::Read;
use std::io::Seek;

#[distributed_slice(TCP_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(s) = ConnectionState::new(config) {
        Box::new(s)
    } else {
        Box::new(Nop::new(config))
    }
}

const NAME: &str = "tcp_connection_state";

pub struct ConnectionState {
    prev: Instant,
    next: Instant,
    interval: Duration,
    files: Vec<File>,
    gauges: Vec<(&'static Lazy<Gauge>, i64)>,
}

impl ConnectionState {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let now = Instant::now();

        let gauges = vec![
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

        let ipv4 = File::open("/proc/net/tcp").map_err(|e| {
            error!("Failed to open /proc/net/tcp: {e}");
        });

        let ipv6 = File::open("/proc/net/tcp6").map_err(|e| {
            error!("Failed to open /proc/net/tcp6: {e}");
        });

        let mut files: Vec<Result<File, ()>> = vec![ipv4, ipv6];

        let files: Vec<File> = files.drain(..).filter_map(|v| v.ok()).collect();

        if files.is_empty() {
            error!("Could not open any file in /proc/net for this sampler");
            return Err(());
        }

        Ok(Self {
            files,
            gauges,
            prev: now,
            next: now,
            interval: config.interval(NAME),
        })
    }
}

impl Sampler for ConnectionState {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        // zero the temporary gauges
        for (_, gauge) in self.gauges.iter_mut() {
            *gauge = 0;
        }

        for file in self.files.iter_mut() {
            // seek to start to cause reload of content
            if file.rewind().is_ok() {
                let mut data = String::new();
                if file.read_to_string(&mut data).is_err() {
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

        // determine when to sample next
        let next = self.next + self.interval;

        // it's possible we fell behind
        if next > now {
            // if we didn't, sample at the next planned time
            self.next = next;
        } else {
            // if we did, sample after the interval has elapsed
            self.next = now + self.interval;
        }

        // mark when we last sampled
        self.prev = now;
    }
}
