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

/// Owned context produced by `build_dashboard_context`. Carries
/// everything `generate_section` needs to render any single section on
/// demand without re-deriving the dedup / category-fallback logic.
#[derive(Default, Clone)]
pub struct DashboardContext {
    /// Navigation list including overview, stock sections, and any
    /// service / category sections — same order as what the eager
    /// generator produced.
    pub sections: Vec<Section>,
    pub filesize: Option<u64>,
    /// Deduped (per `service_name`) and category-aware. The category
    /// flow stores the same list as the per-member flow today; the
    /// difference is whether `category` below is set.
    pub service_exts: Vec<(String, ServiceExtension)>,
    pub category: Option<(String, CategoryExtension)>,
    pub throughput_query: Option<String>,
}

/// Build the navigation list and other shared state needed to render
/// dashboard sections lazily. The dedup + category-fallback logic lives
/// here exclusively; `generate_section` consumes the resulting context.
pub fn build_dashboard_context(
    filesize: Option<u64>,
    service_exts: &[(&str, &ServiceExtension)],
    category: Option<(&str, &CategoryExtension)>,
) -> DashboardContext {
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
    // the rendered output stay in agreement (no nav entry for a route the
    // generator can't produce, no orphaned member sections).
    let category_active = category.is_some() && unique_service_exts.len() == 2;

    // Build the section list. In category mode, a single category section
    // replaces the per-member sections; otherwise the per-member loop
    // runs as before.
    let mut sections: Vec<Section> = std::iter::once(Section {
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
        sections.insert(
            1,
            Section {
                name: category_name.to_string(),
                route: format!("/service/{category_name}"),
            },
        );
    } else {
        for (i, (source_name, _)) in unique_service_exts.iter().enumerate() {
            sections.insert(
                1 + i,
                Section {
                    name: source_name.to_string(),
                    route: format!("/service/{source_name}"),
                },
            );
        }
    }

    let throughput_query = unique_service_exts
        .first()
        .and_then(|(_, e)| e.throughput_query())
        .map(str::to_string);

    let owned_service_exts: Vec<(String, ServiceExtension)> = unique_service_exts
        .iter()
        .map(|(name, ext)| ((*name).to_string(), (*ext).clone()))
        .collect();

    let owned_category = if category_active {
        category.map(|(name, ext)| (name.to_string(), ext.clone()))
    } else {
        None
    };

    DashboardContext {
        sections,
        filesize,
        service_exts: owned_service_exts,
        category: owned_category,
        throughput_query,
    }
}

/// Render a single dashboard section by route. Returns `None` for an
/// unknown route — callers (the viewer) treat this as a 404.
///
/// Filesize is not applied — callers that want a filesize on the response
/// should call `view.set_filesize(...)` themselves.
pub fn generate_section(data: &Tsdb, route: &str, ctx: &DashboardContext) -> Option<View> {
    let view = if route == "/overview" {
        overview::generate(
            data,
            ctx.sections.clone(),
            ctx.throughput_query.as_deref(),
        )
    } else if let Some((_, _, generator)) = SECTION_META.iter().find(|(_, r, _)| *r == route) {
        generator(data, ctx.sections.clone())
    } else if let Some(name) = route.strip_prefix("/service/") {
        // Category route takes precedence when active.
        if let Some((category_name, category_ext)) = &ctx.category {
            if category_name == name && ctx.service_exts.len() == 2 {
                let (a_name, a_ext) = &ctx.service_exts[0];
                let (b_name, b_ext) = &ctx.service_exts[1];
                category::generate(
                    data,
                    ctx.sections.clone(),
                    category_ext,
                    a_name,
                    a_ext,
                    b_name,
                    b_ext,
                )
            } else {
                return None;
            }
        } else if let Some((_, ext)) = ctx.service_exts.iter().find(|(n, _)| n == name) {
            service::generate(data, ctx.sections.clone(), ext)
        } else {
            return None;
        }
    } else {
        return None;
    };

    Some(view)
}

/// Transitional shim preserving the eager `generate` signature so
/// `src/viewer/mod.rs` and `crates/viewer/src/lib.rs` keep building
/// while Tasks B + C migrate them to the lazy API. Remove once those
/// call sites are updated.
#[deprecated(note = "use build_dashboard_context + generate_section")]
pub fn generate(
    data: &Tsdb,
    filesize: Option<u64>,
    service_exts: &[(&str, &ServiceExtension)],
    category: Option<(&str, &CategoryExtension)>,
    _descriptions: Option<&std::collections::HashMap<String, String>>,
) -> std::collections::HashMap<String, String> {
    let ctx = build_dashboard_context(filesize, service_exts, category);

    let mut rendered = std::collections::HashMap::new();

    // Iterate the section nav and render each one lazily. The map keys
    // mirror the original layout: `<route-without-leading-slash>.json`,
    // with `/` preserved in service routes (e.g. `service/vllm.json`).
    for section in &ctx.sections {
        if let Some(mut view) = generate_section(data, &section.route, &ctx) {
            // Match old behavior: filesize only on overview + SECTION_META routes,
            // not on service/category routes.
            let is_legacy_filesize_route = section.route == "/overview"
                || SECTION_META.iter().any(|(_, r, _)| *r == section.route);
            if let (Some(size), true) = (filesize, is_legacy_filesize_route) {
                view.set_filesize(size);
            }
            let key = format!("{}.json", &section.route[1..]);
            rendered.insert(key, serde_json::to_string(&view).unwrap());
        }
    }

    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_context_produces_full_navigation() {
        let ctx = build_dashboard_context(None, &[], None);

        // Sections must be: Overview, then SECTION_META in order. No
        // service / category entries when none supplied.
        let mut expected: Vec<(&str, &str)> = vec![("Overview", "/overview")];
        for (name, route, _) in SECTION_META {
            expected.push((name, route));
        }

        let actual: Vec<(&str, &str)> = ctx
            .sections
            .iter()
            .map(|s| (s.name.as_str(), s.route.as_str()))
            .collect();

        assert_eq!(actual, expected);
        assert!(ctx.service_exts.is_empty());
        assert!(ctx.category.is_none());
        assert!(ctx.throughput_query.is_none());
        assert!(ctx.filesize.is_none());
    }

    #[test]
    fn generate_section_renders_known_routes_returns_none_for_unknown() {
        let data = Tsdb::default();
        let ctx = build_dashboard_context(None, &[], None);

        // Overview renders.
        let overview = generate_section(&data, "/overview", &ctx).expect("overview some");
        let overview_json = serde_json::to_string(&overview).unwrap();
        assert!(overview_json.contains("\"groups\""));
        assert!(overview_json.contains("\"sections\""));
        assert!(overview_json.contains("\"interval\""));

        // A SECTION_META route renders.
        let cpu = generate_section(&data, "/cpu", &ctx).expect("cpu some");
        let cpu_json = serde_json::to_string(&cpu).unwrap();
        assert!(cpu_json.contains("\"groups\""));
        assert!(cpu_json.contains("\"sections\""));
        assert!(cpu_json.contains("\"interval\""));

        // Unknown route returns None.
        assert!(generate_section(&data, "/no-such-route", &ctx).is_none());
        // Unknown service route returns None too.
        assert!(generate_section(&data, "/service/missing", &ctx).is_none());
    }

    #[test]
    fn generate_section_renders_service_and_category_routes() {
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

        let data = Tsdb::default();

        // Per-service flow (no category).
        let ctx = build_dashboard_context(None, &[("vllm", &vllm)], None);
        let view = generate_section(&data, "/service/vllm", &ctx).expect("vllm renders");
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("\"service_name\""));
        assert!(json.contains("\"vllm\""));

        // Category flow with two members.
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
        let ctx = build_dashboard_context(
            None,
            &[("vllm", &vllm), ("sglang", &sglang)],
            Some(("inference-library", &category)),
        );
        // Category renders at /service/<category-name>.
        let view = generate_section(&data, "/service/inference-library", &ctx)
            .expect("category renders");
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("inference-library"));
        // Per-member sections are absent in category mode.
        assert!(generate_section(&data, "/service/vllm", &ctx).is_none());
        assert!(generate_section(&data, "/service/sglang", &ctx).is_none());
    }

    #[test]
    #[allow(deprecated)]
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
    #[allow(deprecated)]
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

    #[test]
    fn shim_filesize_applied_only_to_legacy_routes() {
        // Build a context with a service ext so the shim renders a /service/<name> route.
        let svc_json = r#"{"service_name":"vllm","service_metadata":{},"slo":null,"kpis":[]}"#;
        let svc_ext: ServiceExtension = serde_json::from_str(svc_json).unwrap();
        let data = Tsdb::default();
        #[allow(deprecated)]
        let rendered = generate(&data, Some(12_345), &[("vllm", &svc_ext)], None, None);

        // overview and stock sections carry filesize.
        let overview: serde_json::Value =
            serde_json::from_str(rendered.get("overview.json").unwrap()).unwrap();
        assert_eq!(overview["filesize"], serde_json::json!(12_345));

        let cpu: serde_json::Value =
            serde_json::from_str(rendered.get("cpu.json").unwrap()).unwrap();
        assert_eq!(cpu["filesize"], serde_json::json!(12_345));

        // Service routes do NOT carry filesize (preserves pre-Phase-2 behavior).
        let svc: serde_json::Value =
            serde_json::from_str(rendered.get("service/vllm.json").unwrap()).unwrap();
        assert!(svc.get("filesize").is_none(), "service view leaked filesize");
    }
}
