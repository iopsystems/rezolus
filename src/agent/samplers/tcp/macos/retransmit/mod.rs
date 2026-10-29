//! TCP retransmissions on macOS: `tcp_retransmit` from `tcps_sndrexmitpack`
//! in `net.inet.tcp.stats`, the retransmissions of data sent. A
//! retransmission sent with TSO counts once however many segments it spans,
//! so after an RTO this can be fewer than the segments sent again. A
//! retransmitted SYN or SYN-ACK carries no data and is not counted. See
//! `tcp::macos` for the read and its access check.

const NAME: &str = "tcp_retransmit";

use crate::agent::*;

use super::{check, read, Wide};

use std::sync::Mutex;

mod stats {
    include!("../../linux/retransmit/stats.rs");
}

use stats::*;

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    check()?;

    Ok(Some(Box::new(Retransmit::default())))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

#[derive(Default)]
struct Retransmit {
    sndrexmitpack: Mutex<Wide>,
}

#[async_trait]
impl Sampler for Retransmit {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut guard = COUNTERS_ACQ.acquire();

        let Some(s) = read() else {
            return;
        };
        guard.mark_end();

        TCP_RETRANSMIT.set(self.sndrexmitpack.lock().unwrap().update(s.sndrexmitpack));

        guard.finish();
    }
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
}
