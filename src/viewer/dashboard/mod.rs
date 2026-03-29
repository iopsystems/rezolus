use super::*;

mod blockio;
mod cgroups;
mod cpu;
mod gpu;
mod memory;
mod network;
mod overview;
mod query_explorer;
mod rezolus;
mod scheduler;
mod softirq;
mod syscall;

type Generator = fn(&Tsdb, Vec<Section>) -> View;

static SECTION_META: &[(&str, &str, Generator)] = &[
    ("Overview", "/overview", overview::generate),
    ("Query Explorer", "/query", query_explorer::generate),
    ("CPU", "/cpu", cpu::generate),
    ("GPU", "/gpu", gpu::generate),
    ("Memory", "/memory", memory::generate),
    ("Network", "/network", network::generate),
    ("Scheduler", "/scheduler", scheduler::generate),
    ("Syscall", "/syscall", syscall::generate),
    ("Softirq", "/softirq", softirq::generate),
    ("BlockIO", "/blockio", blockio::generate),
    ("cgroups", "/cgroups", cgroups::generate),
    ("Rezolus", "/rezolus", rezolus::generate),
];

pub fn generate(data: Tsdb, filesize: Option<u64>) -> AppState {
    let mut state = AppState::new(data);

    let sections: Vec<Section> = SECTION_META
        .iter()
        .map(|(name, route, _)| Section {
            name: (*name).to_string(),
            route: (*route).to_string(),
        })
        .collect();

    let tsdb = state.tsdb.read();
    for (_, route, generator) in SECTION_META {
        let key = format!("{}.json", &route[1..]);
        let mut view = generator(&tsdb, sections.clone());
        if let Some(size) = filesize {
            view.set_filesize(size);
        }
        state
            .sections
            .insert(key, serde_json::to_string(&view).unwrap());
    }
    drop(tsdb);

    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_expected_keys() {
        let data = Tsdb::default();
        let state = generate(data, None);

        let mut keys: Vec<_> = state.sections.keys().cloned().collect();
        keys.sort();

        assert_eq!(
            keys,
            vec![
                "blockio.json",
                "cgroups.json",
                "cpu.json",
                "gpu.json",
                "memory.json",
                "network.json",
                "overview.json",
                "query.json",
                "rezolus.json",
                "scheduler.json",
                "softirq.json",
                "syscall.json",
            ]
        );
    }
}
