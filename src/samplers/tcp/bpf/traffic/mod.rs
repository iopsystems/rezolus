#[distributed_slice(TCP_BPF_SAMPLERS)]
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
    dist_next: Instant,
    prev: Instant,
    interval: Duration,
    dist_interval: Duration,
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
            dist_next: now,
            interval: Duration::from_millis(1),
            dist_interval: Duration::from_millis(100),
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

        let maps = self.skel.maps();

        let mut key = [0; 4];
        let mut current = [0; 8];

        let mut counters = vec![
            (&mut self.rx_bytes, &TCP_RX_BYTES, &TCP_RX_BYTES_HIST),
            (&mut self.rx_segments, &TCP_RX_SEGS, &TCP_RX_SEGS_HIST),
            (&mut self.tx_bytes, &TCP_TX_BYTES, &TCP_TX_BYTES_HIST),
            (&mut self.tx_segments, &TCP_TX_SEGS, &TCP_TX_SEGS_HIST),
        ];

        let counts = read_counters(maps.counters().fd(), counters.len());

        for (id, (prev, cnt, hist)) in counters.iter_mut().enumerate() {
            if let Some(curr) = counts.get(&id) {
                if let Some(p) = *prev {
                    let delta = curr.wrapping_sub(*p);
                    cnt.add(delta);
                    hist.increment(now, (delta as f64 / elapsed) as _, 1);
                }
                **prev = Some(*curr);
            }
        }

        if now >= self.dist_next {
            let distributions = vec![
                (&mut self.rx_size, maps.rx_size(), &TCP_RX_SIZE),
                (&mut self.tx_size, maps.tx_size(), &TCP_TX_SIZE),
            ];

            for (prev, map, hist) in distributions {
                update_histogram_from_dist(map.fd(), hist, prev);
            }

            let next = self.dist_next + self.dist_interval;

            if next > now {
                self.dist_next = next;
            } else {
                self.dist_next = now + self.dist_interval;
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