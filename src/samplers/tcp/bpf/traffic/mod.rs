#[distributed_slice(TCP_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(Traffic::new(config))
}

mod traffic_bpf;

use traffic_bpf::*;

use common::bpf::*;
use crate::samplers::tcp::stats::*;
use crate::samplers::tcp::*;
use crate::*;

/// Collects TCP Traffic stats using the following kprobes:
/// * "kprobe/tcp_sendmsg"
/// * "kprobe/tcp_cleanup_rbuf"
pub struct Traffic {
    skel: TrafficSkel<'static>,
    next: Instant,
    prev: Instant,
    interval: Duration,
    rx_bytes: Option<u64>,
    rx_segments: Option<u64>,
    rx_size: [u64; 496],
    tx_bytes: Option<u64>,
    tx_segments: Option<u64>,
    tx_size: [u64; 496],
}

impl Traffic {
    pub fn new(config: &Config) -> Self {
        let now = Instant::now();

        let mut builder = TrafficSkelBuilder::default();
        let mut skel = builder.open().expect("failed to open bpf builder").load().expect("failed to load bpf program");
        skel.attach().expect("failed to attach bpf");

        Self {
            skel,
            next: now,
            prev: now,
            interval: Duration::from_millis(1),
            rx_bytes: None,
            rx_segments: None,
            rx_size: [0; 496],
            tx_bytes: None,
            tx_segments: None,
            tx_size: [0; 496],
        }
    }   
}

impl Sampler for Traffic {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        SAMPLERS_TCP_BPF_TRAFFIC_SAMPLE.increment();

        let elapsed = (now - self.prev).as_secs_f64();

        let mut maps = self.skel.maps();

        let mut current = [0; 8];

        let counters = vec![
            (&mut self.rx_bytes, maps.rx_bytes(), &TCP_RX_BYTES, &TCP_RX_BYTES_HIST),
            (&mut self.rx_segments, maps.rx_segments(), &TCP_RX_SEGS, &TCP_RX_SEGS_HIST),
            (&mut self.tx_bytes, maps.tx_bytes(), &TCP_TX_BYTES, &TCP_TX_BYTES_HIST),
            (&mut self.tx_segments, maps.tx_segments(), &TCP_TX_SEGS, &TCP_TX_SEGS_HIST),
        ];

        for (prev, map, cnt, hist) in counters {
            if let Ok(Some(c)) = map.lookup(&0_u32.to_ne_bytes(), libbpf_rs::MapFlags::ANY) {
                current.copy_from_slice(&c);
                let curr = u64::from_ne_bytes(current);

                if let Some(p) = *prev {
                    let delta = curr.wrapping_sub(p);
                    cnt.add(delta);
                    hist.increment(now, (delta as f64 / elapsed) as _, 1);
                }
                *prev = Some(curr);
            }
        }

        let distributions = vec![
            (&mut self.rx_size, maps.rx_size(), &TCP_RX_SIZE),
            (&mut self.tx_size, maps.tx_size(), &TCP_TX_SIZE),
        ];

        for (prev, map, hist) in distributions {
            for i in 0_u32..496_u32 {
                if let Ok(Some(c)) = map.lookup(&i.to_ne_bytes(), libbpf_rs::MapFlags::ANY) {
                    // convert the index to a usize, as we use it a few
                    // times to index into slices
                    let i = i as usize;

                    // convert bytes to the current count of the bucket
                    current.copy_from_slice(&c);
                    let current = u64::from_ne_bytes(current);

                    // calculate the delta from previous count
                    let delta = current.wrapping_sub(prev[i]);

                    // update the previous count
                    prev[i] = current;

                    // update the heatmap
                    if delta > 0 {
                        let value = key_to_value(i as u64);
                        hist.increment(now, value as _, delta as _);
                    }
                }
            }
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

impl std::fmt::Display for Traffic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "tcp::classic::bpf::traffic")
    }
}