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
mod service;
mod softirq;
mod syscall;

type Generator = fn(&Tsdb, Vec<Section>) -> View;

// Overview is excluded here because it needs an extra throughput_query parameter
// when a service extension is present. It is generated separately in generate().
static SECTION_META: &[(&str, &str, Generator)] = &[
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

pub fn generate(
    data: Tsdb,
    filesize: Option<u64>,
    service_exts: &[(&str, &crate::viewer::ServiceExtension)],
    templates: crate::viewer::TemplateRegistry,
) -> AppState {
    let state = AppState::new(data, templates);

    let mut all_sections: Vec<Section> = std::iter::once(Section {
        name: "Overview".to_string(),
        route: "/overview".to_string(),
    })
    .chain(SECTION_META.iter().map(|(name, route, _)| Section {
        name: (*name).to_string(),
        route: (*route).to_string(),
    }))
    .collect();

    // Insert one service section per source after Overview (position 1+).
    // Use the parquet source name (e.g. "llm-perf") so the sidebar
    // labels the section after the service rather than a generic "Service".
    for (i, (source_name, _)) in service_exts.iter().enumerate() {
        all_sections.insert(
            1 + i,
            Section {
                name: source_name.to_string(),
                route: format!("/service/{source_name}"),
            },
        );
    }

    let tsdb = state.tsdb.read();
    let mut rendered_sections = state.sections.write();

    // Generate overview separately (needs throughput_query from service extension).
    // Use the first service extension's throughput query if available.
    let throughput_query = service_exts
        .first()
        .and_then(|(_, e)| e.throughput_query())
        .map(str::to_string);
    {
        let mut view = overview::generate(&tsdb, all_sections.clone(), throughput_query.as_deref());
        if let Some(size) = filesize {
            view.set_filesize(size);
        }
        rendered_sections.insert(
            "overview.json".to_string(),
            serde_json::to_string(&view).unwrap(),
        );
    }

    for (_, route, generator) in SECTION_META {
        let key = format!("{}.json", &route[1..]);
        let mut view = generator(&tsdb, all_sections.clone());
        if let Some(size) = filesize {
            view.set_filesize(size);
        }
        rendered_sections.insert(key, serde_json::to_string(&view).unwrap());
    }

    // Generate one service section per source
    for (source_name, ext) in service_exts {
        let view = service::generate(&tsdb, all_sections.clone(), ext);
        let key = format!("service/{source_name}.json");
        rendered_sections.insert(key, serde_json::to_string(&view).unwrap());
    }

    drop(rendered_sections);
    drop(tsdb);

    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_expected_keys() {
        let data = Tsdb::default();
        let state = generate(data, None, &[], crate::viewer::TemplateRegistry::empty());

        let mut keys: Vec<_> = state.sections.read().keys().cloned().collect();
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
