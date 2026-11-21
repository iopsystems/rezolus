const NAME: &str = "cpu_cores";

use crate::agent::pmu::parse_cpu_list;
use crate::agent::*;

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

mod stats;

use stats::*;

/// The kernel's list of CPUs isolated from scheduler load balancing: those
/// named by `isolcpus=` with the `domain` flag (the default). A task's affinity
/// mask still includes them, so `nproc` and `taskset` count them, but the load
/// balancer places no work there.
const ISOLATED: &str = "/sys/devices/system/cpu/isolated";

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let online = std::fs::File::open("/sys/devices/system/cpu/online")?;
    // Absent on a kernel without the file; `cpu_isolated` is then never set.
    let isolated = std::fs::File::open(ISOLATED).ok();

    Ok(Some(Box::new(Cores {
        online: Mutex::new(File::from_std(online)),
        isolated: isolated.map(|f| Mutex::new(File::from_std(f))),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

pub struct Cores {
    online: Mutex<File>,
    isolated: Option<Mutex<File>>,
}

async fn read_list(file: &Mutex<File>) -> std::io::Result<String> {
    let mut file = file.lock().await;
    file.rewind().await?;
    let mut raw = String::new();
    file.read_to_string(&mut raw).await?;
    Ok(raw)
}

/// The number of online CPUs, and of online CPUs that are also isolated, from
/// the two sysfs lists. `None` when either list does not parse. An isolated CPU
/// that is offline is not counted, so `cpu_cores - cpu_isolated` is the number
/// of online CPUs the load balancer uses.
fn counts(online: &str, isolated: Option<&str>) -> Option<(usize, Option<usize>)> {
    let online = parse_cpu_list(online)?;
    let isolated = match isolated {
        Some(list) => {
            let isolated = parse_cpu_list(list)?;
            Some(
                isolated
                    .iter()
                    .filter(|c| online.binary_search(c).is_ok())
                    .count(),
            )
        }
        None => None,
    };
    Some((online.len(), isolated))
}

#[async_trait]
impl Sampler for Cores {
    fn name(&self) -> &'static str {
        NAME
    }

    // Acquisition-group bracket (principle 18): acquire before the reads,
    // finish after both `set()`s. A read error or a list that does not parse
    // drops the guard without `finish()` and sets nothing.
    async fn refresh(&self) {
        let guard = CPU_CORES_ACQ.acquire();

        let Ok(online) = read_list(&self.online).await else {
            return;
        };
        let isolated = match &self.isolated {
            Some(file) => match read_list(file).await {
                Ok(list) => Some(list),
                Err(_) => return,
            },
            None => None,
        };
        let Some((online, isolated)) = counts(&online, isolated.as_deref()) else {
            return;
        };

        CPU_CORES.set(online as _);
        if let Some(isolated) = isolated {
            CPU_ISOLATED.set(isolated as _);
        }
        guard.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::counts;

    #[test]
    fn isolated_cpus_are_counted_only_while_online() {
        // isolcpus=domain,0-23,28-55,60-63 on 64 CPUs: 8 left to the balancer.
        assert_eq!(
            counts("0-63\n", Some("0-23,28-55,60-63\n")),
            Some((64, Some(56)))
        );
        // No isolcpus: the file holds an empty line.
        assert_eq!(counts("0-31\n", Some("\n")), Some((32, Some(0))));
        // CPUs 4-7 taken offline: online `0-3,8-15`, isolated `4-11`.
        assert_eq!(counts("0-3,8-15\n", Some("4-11\n")), Some((12, Some(4))));
        // No `isolated` file.
        assert_eq!(counts("0-7\n", None), Some((8, None)));
        // Either list unparseable: nothing is set.
        assert_eq!(counts("0-7,x\n", Some("\n")), None);
        assert_eq!(counts("0-7\n", Some("3-1\n")), None);
    }
}
