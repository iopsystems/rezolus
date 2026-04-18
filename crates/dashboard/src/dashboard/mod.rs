use crate::Tsdb;
use crate::plot::*;
use crate::service_extension::ServiceExtension;

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
    data: &Tsdb,
    filesize: Option<u64>,
    service_exts: &[(&str, &ServiceExtension)],
    _descriptions: Option<&std::collections::HashMap<String, String>>,
) -> std::collections::HashMap<String, String> {
    let mut all_sections: Vec<Section> = std::iter::once(Section {
        name: "Overview".to_string(),
        route: "/overview".to_string(),
    })
    .chain(SECTION_META.iter().map(|(name, route, _)| Section {
        name: (*name).to_string(),
        route: (*route).to_string(),
    }))
    .collect();

    for (i, (source_name, _)) in service_exts.iter().enumerate() {
        all_sections.insert(
            1 + i,
            Section {
                name: source_name.to_string(),
                route: format!("/service/{source_name}"),
            },
        );
    }

    let mut rendered = std::collections::HashMap::new();

    let throughput_query = service_exts
        .first()
        .and_then(|(_, e)| e.throughput_query())
        .map(str::to_string);
    {
        let mut view = overview::generate(data, all_sections.clone(), throughput_query.as_deref());
        if let Some(size) = filesize {
            view.set_filesize(size);
        }
        rendered.insert(
            "overview.json".to_string(),
            serde_json::to_string(&view).unwrap(),
        );
    }

    for (_, route, generator) in SECTION_META {
        let key = format!("{}.json", &route[1..]);
        let mut view = generator(data, all_sections.clone());
        if let Some(size) = filesize {
            view.set_filesize(size);
        }
        rendered.insert(key, serde_json::to_string(&view).unwrap());
    }

    for (source_name, ext) in service_exts {
        let view = service::generate(data, all_sections.clone(), ext);
        let key = format!("service/{source_name}.json");
        rendered.insert(key, serde_json::to_string(&view).unwrap());
    }

    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_expected_keys() {
        let data = Tsdb::default();
        let result = generate(&data, None, &[], None);

        let mut keys: Vec<_> = result.keys().cloned().collect();
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
