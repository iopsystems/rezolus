#[distributed_slice(TCP_BPF_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(RcvEstablished::new(config))
}

mod bpf;

use bpf::*;

use common::{Counter, Distribution};
use super::super::stats::*;
use super::super::*;

/// Collects TCP Traffic stats using BPF
/// Probes:
/// * "tcp_rcv_established"
///
/// Stats:
/// * tcp/receive/jitter
/// * tcp/receive/srtt
pub struct RcvEstablished {
    skel: ModSkel<'static>,
    // counters: Vec<Counter>,
    distributions: Vec<Distribution>,

    next: Instant,
    dist_next: Instant,
    prev: Instant,
    interval: Duration,
    dist_interval: Duration,
}

impl RcvEstablished {
    pub fn new(_config: &Config) -> Self {
        let now = Instant::now();

        let builder = ModSkelBuilder::default();
        let mut skel = builder.open().expect("failed to open bpf builder").load().expect("failed to load bpf program");
        skel.attach().expect("failed to attach bpf");

        // // these need to be in the same order as in the bpf
        // let counters = vec![
        //     Counter::new(&TCP_RX_BYTES, Some(&TCP_RX_BYTES_HIST)),
        //     Counter::new(&TCP_TX_BYTES, Some(&TCP_TX_BYTES_HIST)),
        //     Counter::new(&TCP_RX_SEGMENTS, Some(&TCP_RX_SEGMENTS_HIST)),
        //     Counter::new(&TCP_TX_SEGMENTS, Some(&TCP_TX_SEGMENTS_HIST)),
        // ];

        let distributions = vec![
            Distribution::new("jitter", &TCP_JITTER),
            Distribution::new("srtt", &TCP_SRTT)
        ];

        Self {
            skel,
            // counters,
            distributions,
            next: now,
            prev: now,
            dist_next: now,
            interval: Duration::from_millis(1),
            dist_interval: Duration::from_millis(100),
        }
    }   
}

impl Sampler for RcvEstablished {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        // let elapsed = (now - self.prev).as_secs_f64();

        // let maps = self.skel.maps();

        // let counts = crate::common::bpf::read_counters(maps.counters(), self.counters.len());

        // for (id, counter) in self.counters.iter_mut().enumerate() {
        //     if let Some(current) = counts.get(&id) {
        //         counter.update(now, elapsed, *current);
        //     }
        // }

        // determine if we should sample the distributions
        if now >= self.dist_next {
            for distribution in self.distributions.iter_mut() {
                distribution.update(&self.skel.obj);
            }

            // determine when to sample next
            let next = self.dist_next + self.dist_interval;

            // check that next sample time is in the future
            if next > now {
                self.dist_next = next;
            } else {
                self.dist_next = now + self.dist_interval;
            }
        }

        // determine when to sample next
        let next = self.next + self.interval;
        
        // check that next sample time is in the future
        if next > now {
            self.next = next;
        } else {
            self.next = now + self.interval;
        }

        // mark when we last sampled
        self.prev = now;
    }
}

impl std::fmt::Display for RcvEstablished {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "tcp::bpf::traffic")
    }
}