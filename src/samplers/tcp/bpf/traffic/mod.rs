#[distributed_slice(TCP_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    Box::new(Traffic::new(config))
}

/// Collects TCP Traffic stats using the following kprobes:
/// * "kprobe/tcp_sendmsg"
/// * "kprobe/tcp_cleanup_rbuf"
pub struct Traffic<'a> {
    skel: TrafficSkel<'a>,
    rx_bytes: u64,
    rx_packets: u64,
    rx_size: [u64; 496],
    tx_bytes: u64,
    tx_packets: u64,
    tx_size: [u64; 496],
}

impl<'a> Traffic<'a> {
    pub fn new(config: &Config) -> Self {
        let mut builder = TrafficSkelBuilder::default();
        let mut skel = builder.open()?.load()?;
        skel.attach()?;

        Self {
            skel,
            rx_bytes: 0,
            rx_packets: 0,
            rx_size: [0; 496],
            tx_bytes: 0,
            tx_packets: 0,
            tx_size: [0; 496],
        }
    }
    
}