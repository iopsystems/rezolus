use super::*;

mod blockio;
mod cgroups;
mod cpu;
mod network;
mod overview;
mod scheduler;
mod softirq;
mod syscall;

pub fn generate(data: &Tsdb) -> AppState {
    let mut state = AppState::new();

    // define our sections
    let sections = vec![
        Section {
            name: "Overview".to_string(),
            route: "/overview".to_string(),
        },
        Section {
            name: "CPU".to_string(),
            route: "/cpu".to_string(),
        },
        Section {
            name: "Network".to_string(),
            route: "/network".to_string(),
        },
        Section {
            name: "Scheduler".to_string(),
            route: "/scheduler".to_string(),
        },
        Section {
            name: "Syscall".to_string(),
            route: "/syscall".to_string(),
        },
        Section {
            name: "Softirq".to_string(),
            route: "/softirq".to_string(),
        },
        Section {
            name: "BlockIO".to_string(),
            route: "/blockio".to_string(),
        },
        Section {
            name: "cgroups".to_string(),
            route: "/cgroups".to_string(),
        },
    ];

    state.sections.insert(
        "overview.json".to_string(),
        serde_json::to_string(&overview::generate(data, sections.clone())).unwrap(),
    );
    state.sections.insert(
        "cpu.json".to_string(),
        serde_json::to_string(&cpu::generate(data, sections.clone())).unwrap(),
    );
    state.sections.insert(
        "network.json".to_string(),
        serde_json::to_string(&network::generate(data, sections.clone())).unwrap(),
    );
    state.sections.insert(
        "scheduler.json".to_string(),
        serde_json::to_string(&scheduler::generate(data, sections.clone())).unwrap(),
    );
    state.sections.insert(
        "syscall.json".to_string(),
        serde_json::to_string(&syscall::generate(data, sections.clone())).unwrap(),
    );
    state.sections.insert(
        "softirq.json".to_string(),
        serde_json::to_string(&softirq::generate(data, sections.clone())).unwrap(),
    );
    state.sections.insert(
        "blockio.json".to_string(),
        serde_json::to_string(&blockio::generate(data, sections.clone())).unwrap(),
    );
    state.sections.insert(
        "cgroups.json".to_string(),
        serde_json::to_string(&cgroups::generate(data, sections.clone())).unwrap(),
    );

    state
}
