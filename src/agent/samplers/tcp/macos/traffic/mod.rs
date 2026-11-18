//! TCP traffic on macOS: `tcp_bytes` from `net.inet.tcp.stats`. See
//! `tcp::macos` for the read, its access check, and the widening thread this
//! sampler starts.
//!
//! | metric | XNU counter |
//! |---|---|
//! | `tcp_bytes{direction="transmit"}` | `tcps_sndbyte` |
//! | `tcp_bytes{direction="receive"}` | `tcps_rcvbyte + tcps_rcvoobyte` |
//!
//! `tcps_sndbyte` counts data bytes when they are first sent; retransmitted
//! bytes are not counted. `tcps_rcvbyte` counts the data bytes of segments
//! received in order while nothing is queued for reassembly, and
//! `tcps_rcvoobyte` those of segments received out of order. A segment that
//! starts at the next expected byte while later data is queued for
//! reassembly is in neither, so receive undercounts by those segments,
//! mostly the peer's retransmissions or reordered originals. An
//! out-of-order segment is counted in full before the kernel trims its
//! overlap with segments already queued after it, so overlapping bytes are
//! counted twice.
//!
//! On Linux `tcp_bytes` counts bytes at the socket calls; here it counts them
//! when segments are sent and received. `tcp_packets` and the `tcp_size`
//! histograms are not set: on Linux they count socket calls and their sizes,
//! which the kernel does not count here.

const NAME: &str = "tcp_traffic";

use crate::agent::*;

use super::{check, sample, start_widening, Totals};

mod stats {
    include!("../../linux/traffic/stats.rs");
}

use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    check()?;
    start_widening()?;

    Ok(Some(Box::new(Traffic)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Traffic;

#[async_trait]
impl Sampler for Traffic {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut guard = COUNTERS_ACQ.acquire();

        let Some(t) = sample() else {
            return;
        };
        guard.mark_end();

        set(&t);

        guard.finish();
    }
}

fn set(t: &Totals) {
    TCP_RX_BYTES.set(t.rcvbyte + t.rcvoobyte);
    TCP_TX_BYTES.set(t.sndbyte);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_attribute_to_this_sampler() {
        let module = module_path!().trim_end_matches("::tests");
        let modules = crate::agent::samplers::sampler_modules();
        assert_eq!(
            crate::agent::samplers::attribute_sampler(&format!("{module}::stats"), &modules),
            NAME
        );
        assert_eq!(COUNTERS_ACQ.sampler, NAME);
    }

    #[test]
    fn each_metric_reads_its_own_counters() {
        set(&super::super::tests::distinct_totals());
        assert_eq!(TCP_TX_BYTES.value(), 4);
        assert_eq!(TCP_RX_BYTES.value(), 16 + 256);
    }
}
