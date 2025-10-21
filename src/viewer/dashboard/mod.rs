use super::*;
use std::sync::Arc;

mod blockio;
mod cgroups;
mod cpu;
mod memory;
mod network;
mod overview;
mod query_explorer;
mod rezolus;
mod scheduler;
mod softirq;
mod syscall;

type Generator = fn(&Arc<Tsdb>, Vec<Section>) -> View;

static SECTION_META: &[(&str, &str, Generator)] = &[
    ("Overview", "/overview", overview::generate),
    ("Query Explorer", "/query", query_explorer::generate),
    ("CPU", "/cpu", cpu::generate),
    ("Memory", "/memory", memory::generate),
    ("Network", "/network", network::generate),
    ("Scheduler", "/scheduler", scheduler::generate),
    ("Syscall", "/syscall", syscall::generate),
    ("Softirq", "/softirq", softirq::generate),
    ("BlockIO", "/blockio", blockio::generate),
    ("cgroups", "/cgroups", cgroups::generate),
    ("Rezolus", "/rezolus", rezolus::generate),
];

pub fn generate(data: Arc<Tsdb>) -> AppState {
    let mut state = AppState::new(data.clone());

    let sections: Vec<Section> = SECTION_META
        .iter()
        .map(|(name, route, _)| Section {
            name: (*name).to_string(),
            route: (*route).to_string(),
        })
        .collect();

    for (_, route, generator) in SECTION_META {
        let key = format!("{}.json", &route[1..]);
        let view = generator(&data, sections.clone());
        state
            .sections
            .insert(key, serde_json::to_string(&view).unwrap());
    }

    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_expected_keys() {
        let data = Arc::new(Tsdb::default());
        let state = generate(data);

        let mut keys: Vec<_> = state.sections.keys().cloned().collect();
        keys.sort();

        assert_eq!(
            keys,
            vec![
                "blockio.json",
                "cgroups.json",
                "cpu.json",
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
