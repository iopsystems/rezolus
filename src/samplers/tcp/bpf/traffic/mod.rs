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

        if now >= self.dist_next {
            println!("==================SAMPLE DISTRIBUTIONS=================");
            let distributions = vec![
                (&mut self.rx_size, maps.rx_size(), &TCP_RX_SIZE),
                (&mut self.tx_size, maps.tx_size(), &TCP_TX_SIZE),
            ];

            for (prev, map, hist) in distributions {
                println!("sampling: {}", map.name());
                let mut keys = KEYS.to_owned();
                let mut out: Vec<u8> = vec![0; 496 * 8];
                let mut nkeys: u32 = 496;

                let ret = unsafe {
                    libbpf_sys::bpf_map_lookup_batch(
                        map.fd(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        keys.as_ptr() as *mut core::ffi::c_void,
                        out.as_mut_ptr() as *mut core::ffi::c_void,
                        &mut nkeys as *mut libbpf_sys::__u32,
                        std::ptr::null(),
                    )
                };

                let nkeys = nkeys as usize;

                if ret == 0 {
                    unsafe {
                        out.set_len(8 * nkeys);
                        keys.set_len(4 * nkeys);
                    }
                } else {
                    println!("error: {}", ret);
                    continue;
                }

                println!("nkeys: {}", nkeys);

                for i in 0..nkeys {
                    key.copy_from_slice(&keys[(i * 4)..((i + 1) * 4)]);
                    current.copy_from_slice(&out[(i * 8)..((i + 1) * 8)]);



                    let k = u32::from_ne_bytes(key) as usize;
                    let c = u64::from_ne_bytes(current);

                    println!("key: {} count: {}", k, c);

                    let delta = c.wrapping_sub(prev[k]);
                    prev[k] = c;

                    if delta > 0 {
                        let value = key_to_value(k as u64);
                        hist.increment(now, value as _, delta as _);
                    }
                }
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