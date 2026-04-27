use crate::Tsdb;
use crate::plot::*;
use crate::service_extension::{CategoryExtension, ServiceExtension};

mod blockio;
mod category;
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
    category: Option<(&str, &CategoryExtension)>,
    _descriptions: Option<&std::collections::HashMap<String, String>>,
) -> std::collections::HashMap<String, String> {
    // Two captures of the same service collapse into a single nav entry —
    // both render through the same template and the existing compare-mode
    // overlay handles the per-capture pairing. Without this dedup the nav
    // shows the same route twice and the rendered map double-generates
    // (then HashMap-collapses) the same section.
    let mut seen = std::collections::HashSet::new();
    let unique_service_exts: Vec<(&str, &ServiceExtension)> = service_exts
        .iter()
        .copied()
        .filter(|(name, _)| seen.insert(*name))
        .collect();

    // A category requires exactly two distinct member service exts. If a
    // caller passes Some(category) without that, the category can't be
    // rendered; fall back to per-member sections so the section list and
    // the rendered map stay in agreement (no nav entry for a route the
    // map doesn't have, no orphaned member sections).
    let category_active = category.is_some() && unique_service_exts.len() == 2;

    // Build the section list. In category mode, a single category section
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

    if category_active {
        let (category_name, _) = category.unwrap();
        all_sections.insert(
            1,
            Section {
                name: category_name.to_string(),
                route: format!("/service/{category_name}"),
            },
        );
    } else {
        for (i, (source_name, _)) in unique_service_exts.iter().enumerate() {
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

    let throughput_query = unique_service_exts
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

    if category_active {
        let (category_name, category_ext) = category.unwrap();
        let (a_name, a_ext) = unique_service_exts[0];
        let (b_name, b_ext) = unique_service_exts[1];
        let view = category::generate(
            data,
            all_sections.clone(),
            category_ext,
            a_name,
            a_ext,
            b_name,
            b_ext,
        );
        let key = format!("service/{category_name}.json");
        rendered.insert(key, serde_json::to_string(&view).unwrap());
    } else {
        for (source_name, ext) in &unique_service_exts {
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
    fn generate_emits_category_section_when_category_supplied() {
        use crate::service_extension::{CategoryExtension, CategoryKpi, Kpi, ServiceExtension};
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
        let category = CategoryExtension {
            service_name: "inference-library".to_string(),
            category: true,
            members: vec!["vllm".to_string(), "sglang".to_string()],
            kpis: vec![CategoryKpi {
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
            Some(("inference-library", &category)),
            None,
        );

        // Category section present.
        assert!(result.contains_key("service/inference-library.json"));
        // Per-member sections absent.
        assert!(!result.contains_key("service/vllm.json"));
        assert!(!result.contains_key("service/sglang.json"));
    }

    #[test]
    fn generate_dedupes_section_when_two_captures_share_service_name() {
        use crate::service_extension::{Kpi, ServiceExtension};
        use std::collections::HashMap;

        let kpi = Kpi {
            role: "throughput".to_string(),
            title: "Generation Token Rate".to_string(),
            description: None,
            query: "vllm_q".to_string(),
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
        let vllm_a = ServiceExtension {
            service_name: "vllm".to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            slo: None,
            kpis: vec![kpi.clone()],
        };
        let vllm_b = vllm_a.clone();

        let data = Tsdb::default();
        let result = generate(
            &data,
            None,
            &[("vllm", &vllm_a), ("vllm", &vllm_b)],
            None,
            None,
        );

        assert!(result.contains_key("service/vllm.json"));

        let overview_str = result.get("overview.json").expect("overview rendered");
        let overview: serde_json::Value = serde_json::from_str(overview_str).unwrap();
        let sections = overview
            .get("sections")
            .and_then(|s| s.as_array())
            .expect("sections present");
        let vllm_count = sections
            .iter()
            .filter(|s| s.get("route").and_then(|r| r.as_str()) == Some("/service/vllm"))
            .count();
        assert_eq!(
            vllm_count, 1,
            "expected one /service/vllm entry in nav, got {vllm_count}"
        );
    }
}
