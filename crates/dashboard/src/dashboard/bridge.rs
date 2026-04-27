use crate::Tsdb;
use crate::plot::*;
use crate::service_extension::{BridgeExtension, ServiceExtension};

// Wired into the top-level dashboard generator in Task 7. Until then,
// only the cfg(test) module references this fn, which doesn't prevent
// the lib build's dead_code lint. Drop this attribute when Task 7 lands.
#[allow(dead_code)]
pub fn generate(
    data: &Tsdb,
    all_sections: Vec<Section>,
    bridge: &BridgeExtension,
    baseline_member: &str,
    baseline_ext: &ServiceExtension,
    experiment_member: &str,
    experiment_ext: &ServiceExtension,
) -> View {
    let mut view = View::new(data, all_sections);

    view.metadata.insert(
        "service_name".to_string(),
        serde_json::Value::String(bridge.service_name.clone()),
    );
    view.metadata.insert(
        "bridge_members".to_string(),
        serde_json::Value::Array(vec![
            serde_json::Value::String(baseline_member.to_string()),
            serde_json::Value::String(experiment_member.to_string()),
        ]),
    );

    let mut groups: Vec<(String, Group)> = Vec::new();
    let mut unavailable: Vec<serde_json::Value> = Vec::new();

    for kpi in &bridge.kpis {
        let baseline_title = kpi.member_title(baseline_member);
        let experiment_title = kpi.member_title(experiment_member);
        let baseline_kpi = baseline_ext.kpis.iter().find(|k| k.title == baseline_title);
        let experiment_kpi = experiment_ext
            .kpis
            .iter()
            .find(|k| k.title == experiment_title);

        let (baseline_kpi, experiment_kpi) = match (baseline_kpi, experiment_kpi) {
            (Some(a), Some(b)) => (a, b),
            (None, _) => {
                unavailable.push(serde_json::json!({
                    "title": kpi.title,
                    "missing_member": baseline_member,
                }));
                continue;
            }
            (_, None) => {
                unavailable.push(serde_json::json!({
                    "title": kpi.title,
                    "missing_member": experiment_member,
                }));
                continue;
            }
        };

        // Skip when either member marked the KPI unavailable
        // (validate_service_extensions sets this when the metric is
        // missing from the recording). Treat as missing for unavailable
        // tracking, because the user perceives it the same way.
        if !baseline_kpi.available {
            unavailable.push(serde_json::json!({
                "title": kpi.title,
                "missing_member": baseline_member,
            }));
            continue;
        }
        if !experiment_kpi.available {
            unavailable.push(serde_json::json!({
                "title": kpi.title,
                "missing_member": experiment_member,
            }));
            continue;
        }

        let plot_id = format!(
            "kpi-{}-{}",
            kpi.role,
            crate::dashboard::service::slug(&kpi.title)
        );

        let group = match groups.iter_mut().find(|(r, _)| *r == kpi.role) {
            Some((_, g)) => g,
            None => {
                groups.push((
                    kpi.role.clone(),
                    Group::new(
                        crate::dashboard::service::capitalize(&kpi.role),
                        format!("kpi-{}", kpi.role),
                    ),
                ));
                &mut groups.last_mut().unwrap().1
            }
        };

        let opts = match kpi.metric_type.as_str() {
            "gauge" => PlotOpts::gauge(&kpi.title, &plot_id, Unit::Count),
            "histogram" => PlotOpts::histogram(
                &kpi.title,
                &plot_id,
                Unit::Count,
                kpi.subtype.as_deref().unwrap_or("percentiles"),
            ),
            _ => PlotOpts::counter(&kpi.title, &plot_id, Unit::Count),
        };
        let opts = opts.maybe_unit_system(kpi.unit_system.as_deref());
        let opts = match &kpi.percentiles {
            Some(p) => opts.with_percentiles(p.clone()),
            None => opts,
        };

        let sg = match kpi.subgroup.as_deref() {
            Some(name) => {
                if group.find_subgroup(name).is_none() {
                    let new_sg = group.subgroup(name);
                    if let Some(desc) = kpi.subgroup_description.as_deref() {
                        new_sg.describe(desc);
                    }
                    new_sg
                } else {
                    group.find_subgroup(name).unwrap()
                }
            }
            None => group.default_subgroup(),
        };

        let baseline_query = kpi.effective_query(&baseline_kpi.query);
        let experiment_query = kpi.effective_query(&experiment_kpi.query);
        if kpi.full_width {
            sg.plot_promql_full(opts, baseline_query.clone());
        } else {
            sg.plot_promql(opts, baseline_query.clone());
        }
        if experiment_query != baseline_query
            && let Some(plot) = sg.plots_mut_last()
        {
            plot.promql_query_experiment = Some(experiment_query);
        }
    }

    if !unavailable.is_empty() {
        view.metadata.insert(
            "bridge_unavailable".to_string(),
            serde_json::Value::Array(unavailable),
        );
    }

    for (_, group) in groups {
        view.group(group);
    }
    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service_extension::{BridgeKpi, Kpi};
    use std::collections::HashMap;

    fn kpi(role: &str, title: &str, query: &str) -> Kpi {
        Kpi {
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
        }
    }

    fn ext(name: &str, kpis: Vec<Kpi>) -> ServiceExtension {
        ServiceExtension {
            service_name: name.to_string(),
            aliases: vec![],
            service_metadata: HashMap::new(),
            slo: None,
            kpis,
        }
    }

    #[test]
    fn bridge_generate_emits_section_with_paired_queries() {
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
                denominator: true,
                subgroup: None,
                subgroup_description: None,
                full_width: false,
                member_titles: HashMap::new(),
            }],
        };

        let vllm_ext = ext(
            "vllm",
            vec![kpi("throughput", "Generation Token Rate", "vllm_q")],
        );
        let sglang_ext = ext(
            "sglang",
            vec![kpi("throughput", "Generation Token Rate", "sglang_q")],
        );

        let data = Tsdb::default();
        let view = generate(
            &data,
            vec![],
            &bridge,
            "vllm",
            &vllm_ext,
            "sglang",
            &sglang_ext,
        );

        let json = serde_json::to_value(&view).unwrap();
        let groups = json
            .get("groups")
            .and_then(|g| g.as_array())
            .expect("has groups");
        assert_eq!(groups.len(), 1);
        let plots = groups[0]
            .get("subgroups")
            .and_then(|s| s.as_array())
            .and_then(|s| s.first())
            .and_then(|sg| sg.get("plots"))
            .and_then(|p| p.as_array())
            .expect("has plots");
        assert_eq!(plots.len(), 1);
        let plot = &plots[0];
        assert_eq!(plot["promql_query"].as_str(), Some("vllm_q"));
        assert_eq!(plot["promql_query_experiment"].as_str(), Some("sglang_q"));
        assert_eq!(
            plot["opts"]["title"].as_str(),
            Some("Generation Token Rate")
        );
    }

    #[test]
    fn bridge_generate_records_unavailable_when_member_lookup_misses() {
        let bridge = BridgeExtension {
            service_name: "ifx".to_string(),
            bridge: true,
            members: vec!["a".to_string(), "b".to_string()],
            kpis: vec![BridgeKpi {
                role: "throughput".to_string(),
                title: "Token Rate".to_string(),
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
        let a = ext("a", vec![kpi("throughput", "Token Rate", "a_q")]);
        let b = ext("b", vec![]); // missing the bridged title

        let view = generate(&Tsdb::default(), vec![], &bridge, "a", &a, "b", &b);
        let json = serde_json::to_value(&view).unwrap();

        let unavailable = json
            .get("metadata")
            .and_then(|m| m.get("bridge_unavailable"))
            .and_then(|v| v.as_array())
            .expect("bridge_unavailable present");
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0]["title"].as_str(), Some("Token Rate"));
        assert_eq!(unavailable[0]["missing_member"].as_str(), Some("b"));

        // No groups were emitted (the only KPI was skipped).
        let groups = json.get("groups").and_then(|g| g.as_array()).unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn bridge_generate_records_unavailable_when_member_kpi_marked_unavailable() {
        let bridge = BridgeExtension {
            service_name: "ifx".to_string(),
            bridge: true,
            members: vec!["a".to_string(), "b".to_string()],
            kpis: vec![BridgeKpi {
                role: "throughput".to_string(),
                title: "Token Rate".to_string(),
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
        let a = ext("a", vec![kpi("throughput", "Token Rate", "a_q")]);
        let mut b_kpi = kpi("throughput", "Token Rate", "b_q");
        b_kpi.available = false;
        let b = ext("b", vec![b_kpi]);

        let view = generate(&Tsdb::default(), vec![], &bridge, "a", &a, "b", &b);
        let json = serde_json::to_value(&view).unwrap();

        let unavailable = json
            .get("metadata")
            .and_then(|m| m.get("bridge_unavailable"))
            .and_then(|v| v.as_array())
            .expect("bridge_unavailable present");
        assert_eq!(unavailable.len(), 1);
        assert_eq!(unavailable[0]["title"].as_str(), Some("Token Rate"));
        assert_eq!(unavailable[0]["missing_member"].as_str(), Some("b"));

        let groups = json.get("groups").and_then(|g| g.as_array()).unwrap();
        assert!(groups.is_empty());
    }
}
