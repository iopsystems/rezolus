//! This sampler is used to measure CPU L3 cache access and misses. It does this
//! by using two uncore PMUs for each L3 cache domain.
//!
//! This requires that we identify each L3 cache domain but also identify the
//! correct raw perf events to use which are processor dependent.

const NAME: &str = "cpu_l3";

use crate::agent::*;

use perf_event::events::Event;
use perf_event::ReadFormat;
use tokio::sync::Mutex;
use walkdir::WalkDir;

use std::collections::HashSet;

mod stats;

use stats::*;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    let inner = CpuL3Inner::new()?;

    Ok(Some(Box::new(CpuL3 {
        inner: inner.into(),
    })))
}

struct CpuL3 {
    inner: Mutex<CpuL3Inner>,
}

#[async_trait]
impl Sampler for CpuL3 {
    async fn refresh(&self) {
        let mut inner = self.inner.lock().await;

        let _ = inner.refresh().await;
    }
}

struct CpuL3Inner {
    caches: Vec<L3Cache>,
}

impl CpuL3Inner {
    pub fn new() -> Result<Self, std::io::Error> {
        let caches = get_l3_caches()?;

        Ok(Self { caches })
    }

    pub async fn refresh(&mut self) -> Result<(), std::io::Error> {
        for cache in &mut self.caches {
            if let Ok(group) = cache.access.read_group() {
                if let (Some(access), Some(miss)) =
                    (group.get(&cache.access), group.get(&cache.miss))
                {
                    let access = access.value();
                    let miss = miss.value();

                    for cpu in &cache.shared_cores {
                        let _ = CPU_L3_ACCESS.set(*cpu, access);
                        let _ = CPU_L3_MISS.set(*cpu, miss);
                    }
                }
            }
        }

        Ok(())
    }
}

pub struct LowLevelEvent {
    event_type: u32,
    config: u64,
}

impl LowLevelEvent {
    pub fn new(event_type: u32, config: u64) -> Self {
        Self { event_type, config }
    }
}

impl Event for LowLevelEvent {
    fn update_attrs(self, attr: &mut perf_event_open_sys::bindings::perf_event_attr) {
        attr.type_ = self.event_type;
        attr.config = self.config;
    }
}

/// A struct that contains the perf counters for each L3 cache as well as the
/// list of all CPUs in that L3 domain.
struct L3Cache {
    /// perf events for this cache
    access: perf_event::Counter,
    miss: perf_event::Counter,
    /// all cores which share this cache
    shared_cores: Vec<usize>,
}

impl L3Cache {
    pub fn new(shared_cores: Vec<usize>) -> Result<Self, ()> {
        let cpu = *shared_cores.first().expect("empty l3 domain");

        let (access_event, miss_event) = if let Some(events) = get_events() {
            events
        } else {
            return Err(());
        };

        if let Ok(mut access) = perf_event::Builder::new(access_event)
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .pinned(true)
            .read_format(
                ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING | ReadFormat::GROUP,
            )
            .build()
        {
            if let Ok(miss) = perf_event::Builder::new(miss_event)
                .one_cpu(cpu)
                .any_pid()
                .exclude_hv(false)
                .exclude_kernel(false)
                .build_with_group(&mut access)
            {
                match access.enable_group() {
                    Ok(_) => {
                        return Ok(L3Cache {
                            access,
                            miss,
                            shared_cores,
                        });
                    }
                    Err(e) => {
                        error!("failed to enable the perf group on CPU{cpu}: {e}");
                    }
                }
            }
        }

        Err(())
    }
}

fn l3_domains() -> Result<Vec<Vec<usize>>, std::io::Error> {
    let mut l3_domains = Vec::new();
    let mut processed = HashSet::new();

    // walk the cpu devices directory
    for entry in WalkDir::new("/sys/devices/system/cpu")
        .min_depth(1)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let filename = path.file_name().and_then(|v| v.to_str()).unwrap_or("");

        // check if this is a cpu directory
        if filename.starts_with("cpu") && filename[3..].chars().all(char::is_numeric) {
            let cache_dir = path.join("cache");

            // look for the cache where level = 3
            if let Some(l3_index) = WalkDir::new(&cache_dir)
                .into_iter()
                .filter_map(|e| e.ok())
                .find(|entry| {
                    let index_path = entry.path();
                    index_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|name| {
                            name.starts_with("index")
                                && index_path.join("level").exists()
                                && std::fs::read_to_string(index_path.join("level"))
                                    .unwrap_or_default()
                                    .trim()
                                    == "3"
                        })
                })
            {
                let shared_cpu_list = l3_index.path().join("shared_cpu_list");

                // parse the shared cpu list
                if let Ok(shared_cpu_list) = std::fs::read_to_string(&shared_cpu_list) {
                    let shared_cores = parse_cpu_list(&shared_cpu_list);

                    // avoid duplicates
                    if !processed.contains(&shared_cores) {
                        processed.insert(shared_cores.clone());
                        l3_domains.push(shared_cores);
                    }
                }
            }
        }
    }

    Ok(l3_domains)
}

fn get_l3_caches() -> Result<Vec<L3Cache>, std::io::Error> {
    let mut l3_domains = l3_domains()?;

    let mut l3_caches = Vec::new();

    for l3_domain in l3_domains.drain(..) {
        if let Ok(l3_cache) = L3Cache::new(l3_domain) {
            l3_caches.push(l3_cache);
        }
    }

    Ok(l3_caches)
}

fn parse_cpu_list(list: &str) -> Vec<usize> {
    let mut cores = Vec::new();

    for range in list.trim().split(',') {
        if let Some((start, end)) = range.split_once('-') {
            // Range of cores
            if let (Ok(start_num), Ok(end_num)) = (start.parse::<usize>(), end.parse::<usize>()) {
                cores.extend(start_num..=end_num);
            }
        } else {
            // Single core
            if let Ok(core) = range.parse::<usize>() {
                cores.push(core);
            }
        }
    }

    cores.sort_unstable();
    cores.dedup();
    cores
}

fn get_events() -> Option<(LowLevelEvent, LowLevelEvent)> {
    if let Ok(uarch) = archspec::cpu::host().map(|u| u.name().to_owned()) {
        let events = match uarch.as_str() {
            "zen" | "zen2" | "zen3" | "zen4" | "zen5" => (
                LowLevelEvent::new(0xb, 0xFF04),
                LowLevelEvent::new(0xb, 0x0104),
            ),
            _ => {
                return None;
            }
        };

        Some(events)
    } else {
        None
    }
}
