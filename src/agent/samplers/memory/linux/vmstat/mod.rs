//! Memory-management counters from `/proc/vmstat`, one read and parse per
//! refresh: NUMA allocation placement, page-cache writeback totals, reclaim,
//! page faults, swap, working-set refaults, OOM kills, transparent huge pages,
//! compaction and NUMA balancing.
//!
//! Every value in the file is a count in pages or events, except the two
//! dirty thresholds, which are the page counts the writeback throttle
//! compares `Dirty` against and are exported as byte gauges. A line the
//! running kernel does not print (`numa_*` without NUMA, `thp_*` without
//! `CONFIG_TRANSPARENT_HUGEPAGE`, the `workingset_*_anon` split before 5.9)
//! leaves its metric never set, which the snapshot reports as absent rather
//! than 0.
//!
//! Metric names carry the memory subsystem, not the file: a reader should not
//! have to know whether a number came from `/proc/vmstat` or `/proc/meminfo`.

const NAME: &str = "memory_vmstat";

use crate::agent::*;

use metriken::{LazyCounter, LazyGauge};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

use std::collections::HashMap;

mod stats;

use stats::*;

/// One `/proc/vmstat` key to one counter.
const COUNTERS: &[(&str, &LazyCounter)] = &[
    // NUMA allocation placement
    ("numa_hit", &MEMORY_NUMA_HIT),
    ("numa_miss", &MEMORY_NUMA_MISS),
    ("numa_foreign", &MEMORY_NUMA_FOREIGN),
    ("numa_interleave", &MEMORY_NUMA_INTERLEAVE),
    ("numa_local", &MEMORY_NUMA_LOCAL),
    ("numa_other", &MEMORY_NUMA_OTHER),
    // page-cache writeback totals
    ("nr_dirtied", &MEMORY_PAGES_DIRTIED),
    ("nr_written", &MEMORY_PAGES_WRITTEN),
    // reclaim
    ("pgscan_kswapd", &MEMORY_RECLAIM_SCANNED_KSWAPD),
    ("pgscan_direct", &MEMORY_RECLAIM_SCANNED_DIRECT),
    ("pgsteal_kswapd", &MEMORY_RECLAIM_RECLAIMED_KSWAPD),
    ("pgsteal_direct", &MEMORY_RECLAIM_RECLAIMED_DIRECT),
    // faults
    ("pgfault", &MEMORY_PAGE_FAULTS),
    ("pgmajfault", &MEMORY_MAJOR_PAGE_FAULTS),
    // swap
    ("pswpin", &MEMORY_SWAP_IN),
    ("pswpout", &MEMORY_SWAP_OUT),
    // working set (the file/anon split exists from 5.9)
    ("workingset_refault_file", &MEMORY_WORKINGSET_REFAULTS_FILE),
    ("workingset_refault_anon", &MEMORY_WORKINGSET_REFAULTS_ANON),
    (
        "workingset_activate_file",
        &MEMORY_WORKINGSET_ACTIVATIONS_FILE,
    ),
    (
        "workingset_activate_anon",
        &MEMORY_WORKINGSET_ACTIVATIONS_ANON,
    ),
    ("workingset_restore_file", &MEMORY_WORKINGSET_RESTORES_FILE),
    ("workingset_restore_anon", &MEMORY_WORKINGSET_RESTORES_ANON),
    // OOM
    ("oom_kill", &MEMORY_OOM_KILLS),
    // transparent huge pages
    ("thp_fault_alloc", &MEMORY_THP_FAULTS_ALLOCATED),
    ("thp_fault_fallback", &MEMORY_THP_FAULTS_FALLBACK),
    ("thp_collapse_alloc", &MEMORY_THP_COLLAPSES),
    ("thp_split_page", &MEMORY_THP_SPLITS),
    // compaction
    ("compact_stall", &MEMORY_COMPACTION_STALLS),
    ("compact_success", &MEMORY_COMPACTIONS_SUCCESS),
    ("compact_fail", &MEMORY_COMPACTIONS_FAIL),
    // NUMA balancing
    ("numa_pte_updates", &MEMORY_NUMA_BALANCING_PTE_UPDATES),
    ("numa_hint_faults", &MEMORY_NUMA_BALANCING_HINT_FAULTS),
    ("numa_pages_migrated", &MEMORY_NUMA_BALANCING_PAGES_MIGRATED),
];

/// Several keys summed into one counter: the kernel reports allocation
/// stalls per zone, and which zones exist depends on the machine.
const SUMMED: &[(&[&str], &LazyCounter)] = &[(
    &[
        "allocstall_dma",
        "allocstall_dma32",
        "allocstall_normal",
        "allocstall_high",
        "allocstall_movable",
        "allocstall_device",
    ],
    &MEMORY_ALLOCATION_STALLS,
)];

/// Page counts exported as byte gauges.
const GAUGES: &[(&str, &LazyGauge)] = &[
    ("nr_dirty_threshold", &MEMORY_DIRTY_THRESHOLD_HARD),
    (
        "nr_dirty_background_threshold",
        &MEMORY_DIRTY_THRESHOLD_BACKGROUND,
    ),
];

fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = VmstatInner::new()?;

    Ok(Some(Box::new(Vmstat {
        inner: inner.into(),
    })))
}

#[distributed_slice(SAMPLERS)]
static SAMPLER_ENTRY: crate::agent::samplers::SamplerEntry = crate::agent::samplers::SamplerEntry {
    name: NAME,
    module: module_path!(),
    init,
};

struct Vmstat {
    inner: Mutex<VmstatInner>,
}

#[async_trait]
impl Sampler for Vmstat {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

struct VmstatInner {
    data: String,
    file: File,
    tables: Tables,
}

/// The parse's lookup structures, built once.
struct Tables {
    counters: HashMap<&'static str, &'static LazyCounter>,
    /// key -> index into `SUMMED`
    summed: HashMap<&'static str, usize>,
    gauges: HashMap<&'static str, &'static LazyGauge>,
    page_size: i64,
}

impl Tables {
    fn new(page_size: i64) -> Self {
        let counters = COUNTERS.iter().copied().collect();
        let summed = SUMMED
            .iter()
            .enumerate()
            .flat_map(|(i, (keys, _))| keys.iter().map(move |k| (*k, i)))
            .collect();
        let gauges = GAUGES.iter().copied().collect();

        Self {
            counters,
            summed,
            gauges,
            page_size,
        }
    }
}

impl VmstatInner {
    pub fn new() -> Result<Self, std::io::Error> {
        let file = std::fs::File::open("/proc/vmstat").map(File::from_std)?;

        // SAFETY: sysconf has no preconditions.
        let page_size = match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
            n if n > 0 => n as i64,
            _ => 4096,
        };

        Ok(Self {
            data: String::new(),
            file,
            tables: Tables::new(page_size),
        })
    }

    // See the identical discard-on-error rationale on `memory_meminfo`'s
    // `refresh()`.
    pub async fn refresh(&mut self) -> Result<(), std::io::Error> {
        let guard = VMSTAT_ACQ.acquire();

        self.file.rewind().await?;

        self.data.clear();

        self.file.read_to_string(&mut self.data).await?;

        parse(&self.data, &self.tables);

        guard.finish();

        Ok(())
    }
}

/// Set every metric whose key appears in `data`. Keys the text lacks leave
/// their metric untouched; a summed counter is set only if at least one of
/// its keys appeared, to the sum of those that did.
fn parse(data: &str, tables: &Tables) {
    let mut sums: Vec<Option<u64>> = vec![None; SUMMED.len()];

    for line in data.lines() {
        let mut parts = line.split_whitespace();

        let Some(key) = parts.next() else {
            continue;
        };

        let Some(Ok(v)) = parts.next().map(|v| v.parse::<u64>()) else {
            continue;
        };

        if let Some(counter) = tables.counters.get(key) {
            counter.set(v);
        } else if let Some(&i) = tables.summed.get(key) {
            sums[i] = Some(sums[i].unwrap_or(0).saturating_add(v));
        } else if let Some(gauge) = tables.gauges.get(key) {
            gauge.set((v as i64).saturating_mul(tables.page_size));
        }
    }

    for (i, (_, counter)) in SUMMED.iter().enumerate() {
        if let Some(sum) = sums[i] {
            counter.set(sum);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `/proc/vmstat` prints: a key, a space, a number. Zone-split
    /// allocstall lines as a 64-bit x86 machine prints them; the 5.9+
    /// workingset split.
    const SAMPLE: &str = "nr_dirty 49
nr_writeback 0
nr_dirty_threshold 262144
nr_dirty_background_threshold 65536
nr_dirtied 201429
nr_written 189349
numa_hit 1000
numa_miss 10
numa_foreign 10
numa_interleave 5
numa_local 990
numa_other 20
numa_pte_updates 300
numa_hint_faults 200
numa_pages_migrated 100
pgfault 555555
pgmajfault 1234
pswpin 7
pswpout 9
pgscan_kswapd 40000
pgscan_direct 5000
pgsteal_kswapd 30000
pgsteal_direct 4000
allocstall_dma 0
allocstall_dma32 3
allocstall_normal 17
allocstall_movable 2
workingset_refault_anon 11
workingset_refault_file 2200
workingset_activate_anon 3
workingset_activate_file 400
workingset_restore_anon 1
workingset_restore_file 50
oom_kill 1
compact_stall 12
compact_fail 4
compact_success 8
thp_fault_alloc 600
thp_fault_fallback 60
thp_collapse_alloc 9
thp_split_page 2
";

    #[test]
    fn counters_sums_and_gauges_parse() {
        let tables = Tables::new(4096);
        parse(SAMPLE, &tables);
        assert_eq!(MEMORY_PAGES_WRITTEN.value(), 189349);
        assert_eq!(MEMORY_RECLAIM_SCANNED_DIRECT.value(), 5000);
        assert_eq!(MEMORY_RECLAIM_RECLAIMED_KSWAPD.value(), 30000);
        assert_eq!(MEMORY_MAJOR_PAGE_FAULTS.value(), 1234);
        assert_eq!(MEMORY_WORKINGSET_REFAULTS_FILE.value(), 2200);
        assert_eq!(MEMORY_THP_FAULTS_FALLBACK.value(), 60);
        assert_eq!(MEMORY_COMPACTIONS_FAIL.value(), 4);
        assert_eq!(MEMORY_NUMA_BALANCING_PAGES_MIGRATED.value(), 100);
        // allocstall summed across the zones present: 0 + 3 + 17 + 2
        assert_eq!(MEMORY_ALLOCATION_STALLS.value(), 22);
        // thresholds are pages, exported in bytes
        assert_eq!(MEMORY_DIRTY_THRESHOLD_HARD.value(), 262144 * 4096);
        assert_eq!(MEMORY_DIRTY_THRESHOLD_BACKGROUND.value(), 65536 * 4096);
    }

    /// Every key in the tables is one the kernel prints and none is mapped
    /// twice across the three tables: a typo would leave a metric unset
    /// forever, and a duplicate would make two metrics of one line.
    #[test]
    fn every_key_is_in_the_sample_exactly_once() {
        let mut keys: Vec<&str> = COUNTERS.iter().map(|(k, _)| *k).collect();
        keys.extend(SUMMED.iter().flat_map(|(ks, _)| ks.iter().copied()));
        keys.extend(GAUGES.iter().map(|(k, _)| *k));
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total, "a key is mapped twice");
        for key in keys {
            // allocstall_high and allocstall_device are zones this sample's
            // machine does not have; every other key must be present.
            if key == "allocstall_high" || key == "allocstall_device" {
                continue;
            }
            assert!(
                SAMPLE
                    .lines()
                    .any(|l| l.split_whitespace().next() == Some(key)),
                "{key} is not a line /proc/vmstat prints"
            );
        }
    }

    /// A summed counter with none of its keys present stays untouched, and
    /// a plain counter is left alone when its line is missing.
    #[test]
    fn missing_lines_leave_metrics_untouched() {
        let tables = Tables::new(4096);
        MEMORY_ALLOCATION_STALLS.set(41);
        MEMORY_OOM_KILLS.set(3);
        parse("pgfault 1\n", &tables);
        assert_eq!(MEMORY_PAGE_FAULTS.value(), 1);
        assert_eq!(MEMORY_ALLOCATION_STALLS.value(), 41);
        assert_eq!(MEMORY_OOM_KILLS.value(), 3);
    }
}
