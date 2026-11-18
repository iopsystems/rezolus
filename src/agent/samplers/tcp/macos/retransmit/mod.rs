//! TCP retransmissions on macOS: `tcp_retransmit` from `tcps_sndrexmitpack`
//! in `net.inet.tcp.stats`, the retransmissions of data sent. A
//! retransmission sent with TSO counts once however many segments it spans,
//! so after an RTO this can be fewer than the segments sent again. A
//! retransmitted SYN or SYN-ACK carries no data and is not counted. See
//! `tcp::macos` for the read and its access check.

const NAME: &str = "tcp_retransmit";

use crate::agent::*;

use super::{check, sample, Totals};

mod stats {
    include!("../../linux/retransmit/stats.rs");
}

use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    check()?;

    Ok(Some(Box::new(Retransmit)))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Retransmit;

#[async_trait]
impl Sampler for Retransmit {
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
    TCP_RETRANSMIT.set(t.sndrexmitpack);
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
    fn the_metric_reads_its_own_counter() {
        set(&super::super::tests::distinct_totals());
        assert_eq!(TCP_RETRANSMIT.value(), 8);
    }
}
