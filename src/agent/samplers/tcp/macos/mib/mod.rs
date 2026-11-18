//! TCP protocol counters on macOS, from `net.inet.tcp.stats`: delayed ACKs,
//! retransmission timeouts, retransmitted segments and receive errors. See
//! `tcp::macos` for the read and its access check.
//!
//! | metric | XNU counter |
//! |---|---|
//! | `tcp_delayed_acks` | `tcps_delack` |
//! | `tcp_rto_expirations` | `tcps_rexmttimeo` |
//! | `tcp_segments_retransmitted` | `tcps_sndrexmitpack` |
//! | `tcp_checksum_errors` | `tcps_rcvbadsum` |
//! | `tcp_input_errors` | `tcps_rcvbadsum + tcps_rcvbadoff + tcps_rcvshort` |
//!
//! `tcps_sndrexmitpack` counts each retransmission sent, once however many
//! segments a TSO send spans, and data only, so a retransmitted SYN or
//! SYN-ACK is not counted. `tcps_rcvshort` also counts segments the kernel
//! could not pull up for lack of mbufs.

const NAME: &str = "tcp_mib";

use crate::agent::*;

use super::{check, sample, Totals};

mod stats {
    include!("../../linux/mib/stats.rs");
}

use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    check()?;

    Ok(Some(Box::new(Mib)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Mib;

#[async_trait]
impl Sampler for Mib {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        // One read supplies the metrics of both groups.
        let mut netstat = NETSTAT_ACQ.acquire();
        let mut snmp = SNMP_ACQ.acquire();

        let Some(t) = sample() else {
            return;
        };
        netstat.mark_end();
        snmp.mark_end();

        set(&t);

        netstat.finish();
        snmp.finish();
    }
}

fn set(t: &Totals) {
    TCP_DELAYED_ACKS.set(t.delack);
    TCP_RTO_EXPIRATIONS.set(t.rexmttimeo);
    TCP_CHECKSUM_ERRORS.set(t.rcvbadsum);
    TCP_INPUT_ERRORS.set(t.rcvbadsum + t.rcvbadoff + t.rcvshort);
    TCP_SEGMENTS_RETRANSMITTED.set(t.sndrexmitpack);
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
        assert_eq!(NETSTAT_ACQ.sampler, NAME);
        assert_eq!(SNMP_ACQ.sampler, NAME);
    }

    // Each field advances by a different amount, so a metric fed from the
    // wrong field reads a different value.
    #[test]
    fn each_metric_reads_its_own_counter() {
        set(&super::super::tests::distinct_totals());

        assert_eq!(TCP_DELAYED_ACKS.value(), 1);
        assert_eq!(TCP_RTO_EXPIRATIONS.value(), 2);
        assert_eq!(TCP_SEGMENTS_RETRANSMITTED.value(), 8);
        assert_eq!(TCP_CHECKSUM_ERRORS.value(), 32);
        assert_eq!(TCP_INPUT_ERRORS.value(), 32 + 64 + 128);
    }
}
