use crate::Tsdb;
use crate::plot::*;
use crate::service_extension::{BridgeExtension, ServiceExtension};

mod blockio;
mod bridge;
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
    bridge: Option<(&str, &BridgeExtension)>,
    _descriptions: Option<&std::collections::HashMap<String, String>>,
) -> std::collections::HashMap<String, String> {
    // A bridge requires exactly two member service exts. If a caller
    // passes Some(bridge) without that, the bridge can't be rendered;
    // fall back to per-member sections so the section list and the
    // rendered map stay in agreement (no nav entry for a route the map
    // doesn't have, no orphaned member sections).
    let bridge_active = bridge.is_some() && service_exts.len() == 2;

    // Build the section list. In bridge mode, a single bridge section
    // replaces the per-member sections; otherwise the per-member loop
    // runs as before.
    let mut all_sections: Vec<Section> = std::iter::once(Section {
        name: "Overview".to_string(),
        route: "/overview".to_string(),
    })
    .chain(SECTION_META.iter().map(|(name, route, _)| Section {
        name: (*name).to_string(),
        route: (*route).to_string(),
    }))
    .collect();

    if bridge_active {
        let (bridge_name, _) = bridge.unwrap();
        all_sections.insert(
            1,
            Section {
                name: bridge_name.to_string(),
                route: format!("/service/{bridge_name}"),
            },
        );
    } else {
        for (i, (source_name, _)) in service_exts.iter().enumerate() {
            all_sections.insert(
                1 + i,
                Section {
                    name: source_name.to_string(),
                    route: format!("/service/{source_name}"),
                },
            );
        }
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

    if bridge_active {
        let (bridge_name, bridge_ext) = bridge.unwrap();
        let (a_name, a_ext) = service_exts[0];
        let (b_name, b_ext) = service_exts[1];
        let view = bridge::generate(
            data,
            all_sections.clone(),
            bridge_ext,
            a_name,
            a_ext,
            b_name,
            b_ext,
        );
        let key = format!("service/{bridge_name}.json");
        rendered.insert(key, serde_json::to_string(&view).unwrap());
    } else {
        for (source_name, ext) in service_exts {
            let view = service::generate(data, all_sections.clone(), ext);
            let key = format!("service/{source_name}.json");
            rendered.insert(key, serde_json::to_string(&view).unwrap());
        }
    }

    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_expected_keys() {
        let data = Tsdb::default();
        let result = generate(&data, None, &[], None, None);

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

    #[test]
    fn generate_emits_bridge_section_when_bridge_supplied() {
        use crate::service_extension::{BridgeExtension, BridgeKpi, Kpi, ServiceExtension};
        use std::collections::HashMap;

        let kpi = |role: &str, title: &str, query: &str| Kpi {
            role: role.to_string(),
            title: title.to_string(),
            description: None,
            query: query.to_string(),
            metric_type: "delta_counter".to_string(),
            subtype: None,
            unit_system: Some("rate".to_string()),
            percentiles: None,
            available: true,
            denominator: false,
            subgroup: None,
            subgroup_description: None,
            full_width: false,
        };
        let vllm = ServiceExtension {
            service_name: "vllm".to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            slo: None,
            kpis: vec![kpi("throughput", "Generation Token Rate", "vllm_q")],
        };
        let sglang = ServiceExtension {
            service_name: "sglang".to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            slo: None,
            kpis: vec![kpi("throughput", "Generation Token Rate", "sglang_q")],
        };
        let bridge = BridgeExtension {
            service_name: "inference-library".to_string(),
            bridge: true,
            members: vec!["vllm".to_string(), "sglang".to_string()],
            kpis: vec![BridgeKpi {
                role: "throughput".to_string(),
                title: "Generation Token Rate".to_string(),
                metric_type: "delta_counter".to_string(),
                subtype: None,
                unit_system: Some("rate".to_string()),
                percentiles: None,
                denominator: false,
                subgroup: None,
                subgroup_description: None,
                full_width: false,
                member_titles: HashMap::new(),
            }],
        };

        let data = Tsdb::default();
        let result = generate(
            &data,
            None,
            &[("vllm", &vllm), ("sglang", &sglang)],
            Some(("inference-library", &bridge)),
            None,
        );

        // Bridge section present.
        assert!(result.contains_key("service/inference-library.json"));
        // Per-member sections absent.
        assert!(!result.contains_key("service/vllm.json"));
        assert!(!result.contains_key("service/sglang.json"));
    }
}
