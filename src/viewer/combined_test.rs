//! An archive of several recordings of one run, opened as one combined view
//! (`dashboard::combined`): each template once per recording that carries
//! it, each instance reading only its recording's series.

use std::path::{Path, PathBuf};

use metriken_query::{MetricsSource, QueryResult};
use rez::fixtures::{one_run, Recording, ANCHOR, TICKS};

use super::state::AppState;

/// Two Rezolus agents of one run: `h1` with one CPU 25% busy, agent 6.0.0;
/// `h2` with two CPUs 75% busy, agent 5.9.0.
pub(crate) fn two_hosts(dir: &Path) -> PathBuf {
    let path = dir.join("run.dendro");
    one_run(
        &path,
        &[
            Recording {
                labels: &[("source", "rezolus"), ("host", "h1")],
                metadata: &[("source", "rezolus"), ("version", "6.0.0")],
                cpus: 1,
                busy_ns: 250_000_000,
                counter: None,
            },
            Recording {
                labels: &[("source", "rezolus"), ("host", "h2")],
                metadata: &[("source", "rezolus"), ("version", "5.9.0")],
                cpus: 2,
                busy_ns: 750_000_000,
                counter: None,
            },
        ],
    );
    path
}

/// A KPI template for the service `svc`, its throughput a rate of
/// `svc_requests`.
pub(crate) fn svc_template() -> String {
    serde_json::json!({
        "service_name": "svc",
        "kpis": [{
            "role": "throughput",
            "title": "Requests",
            "query": "sum(irate(svc_requests[5m]))",
            "type": "delta_counter",
            "denominator": true,
        }],
    })
    .to_string()
}

/// A Rezolus agent and a service of one run, on one host; the service's
/// recording carries [`svc_template`] as its `service_queries`.
pub(crate) fn agent_and_service(dir: &Path) -> PathBuf {
    let path = dir.join("svc.dendro");
    let template = svc_template();
    one_run(
        &path,
        &[
            Recording {
                labels: &[("source", "rezolus"), ("host", "h1")],
                metadata: &[("source", "rezolus")],
                cpus: 1,
                busy_ns: 250_000_000,
                counter: None,
            },
            Recording {
                labels: &[("source", "svc"), ("host", "h1")],
                metadata: &[("source", "svc"), ("service_queries", &template)],
                cpus: 0,
                busy_ns: 0,
                // Named like an agent metric too, so a query that is not
                // restricted to the agent reads the service's series.
                counter: Some(("svc_requests", 40)),
            },
        ],
    );
    path
}

/// The state `rezolus view <path> <flags>` builds.
pub(crate) fn view(path: &Path, flags: &[&str]) -> AppState {
    let mut argv = vec!["view", path.to_str().unwrap()];
    argv.extend_from_slice(flags);
    let config = super::Config::try_from(super::command().get_matches_from(argv)).unwrap();
    super::init_file_mode(
        &config,
        path,
        &::dashboard::TemplateRegistry::empty(),
        metriken_storage::BufferPool::new(64 << 20),
    )
}

/// The section at `route`, as `/data/<route>.json` serves it.
pub(crate) fn section(state: &AppState, route: &str) -> serde_json::Value {
    let data = state.baseline_data();
    let mut value = state
        .sections
        .read()
        .generate(route, data.as_ref())
        .unwrap_or_else(|| panic!("{route} renders"));
    super::routes::strip_sections_from_section_payload(&mut value);
    value
}

/// Every plot's `promql_query` in a section, by plot id.
fn plot_queries(section: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for group in section["groups"].as_array().unwrap() {
        for sg in group["subgroups"].as_array().unwrap() {
            for plot in sg["plots"].as_array().unwrap() {
                out.push((
                    plot["opts"]["id"].as_str().unwrap().to_string(),
                    plot["promql_query"].as_str().unwrap().to_string(),
                ));
            }
        }
    }
    out
}

/// The last value of each series `query` returns over the archive.
fn last_values(data: &dyn MetricsSource, query: &str) -> Vec<f64> {
    let end = (ANCHOR + (TICKS - 1) * 1_000_000_000) as f64 / 1e9;
    match data.query_range(query, end - 3.0, end, 1.0).unwrap() {
        QueryResult::Matrix { result, .. } => {
            result.iter().map(|s| s.values.last().unwrap().1).collect()
        }
        other => panic!("{query}: {other:?}"),
    }
}

/// Two hosts of one run: one capture, the built-in sections once per host
/// (the second host's routes keyed `@h2`, the query explorer once), each
/// entry naming its host. Each host's CPU section is expanded from that
/// host's catalog (only `h2` has more than one CPU, so only it has a
/// per-CPU chart), carries its own header, and every query in it reads only
/// its host's series.
#[test]
fn each_hosts_cpu_section_reads_only_that_host() {
    let dir = tempfile::tempdir().unwrap();
    let state = view(&two_hosts(dir.path()), &[]);
    assert!(state.captures.get_by_id("experiment").is_none());
    // The archive answers each recording's catalog questions.
    assert!(state.combined_catalog.read().is_some());

    let sections = state.sections_metadata()["sections"].clone();
    let entries: Vec<(&str, &str, Option<&str>)> = sections
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap(),
                s["route"].as_str().unwrap(),
                s["instance"].as_str(),
            )
        })
        .collect();
    assert!(
        entries.contains(&("CPU", "/cpu", Some("h1"))),
        "{entries:?}"
    );
    assert!(
        entries.contains(&("CPU", "/cpu@h2", Some("h2"))),
        "{entries:?}"
    );
    assert!(entries.contains(&("Overview", "/overview@h2", Some("h2"))));
    let queries = entries.iter().filter(|e| e.1.starts_with("/query")).count();
    assert_eq!(queries, 1, "the query explorer renders once: {entries:?}");

    let data = state.baseline_data();
    for (route, host, version, cpus, busy) in [
        ("/cpu", "h1", "6.0.0", 1, 0.25),
        ("/cpu@h2", "h2", "5.9.0", 2, 0.75),
    ] {
        let cpu = section(&state, route);
        assert_eq!(cpu["version"], version, "{route}'s header is its host's");
        let plots = plot_queries(&cpu);
        let per_cpu = plots.iter().any(|(id, _)| id == "busy-pct-per-cpu");
        assert_eq!(
            per_cpu,
            cpus > 1,
            "{route}: per-CPU chart from its own catalog"
        );
        assert_eq!(
            cpu["metadata"]["scope"],
            serde_json::json!([{"label": "host", "op": "=", "value": host}])
        );
        for (id, query) in &plots {
            assert!(
                query.contains(&format!("host=\"{host}\"")),
                "{route} {id}: {query}"
            );
        }
        let (_, busy_query) = plots.iter().find(|(id, _)| id == "busy-pct").unwrap();
        let values = last_values(data.as_ref(), busy_query);
        assert_eq!(values.len(), 1, "{route}: {busy_query} -> {values:?}");
        assert!((values[0] - busy).abs() < 1e-9, "{route}: {values:?}");
    }
}

/// `--baseline`/`--experiment` open the same archive as a comparison.
#[test]
fn a_selector_opens_the_comparison() {
    let dir = tempfile::tempdir().unwrap();
    let state = view(
        &two_hosts(dir.path()),
        &["--baseline", "host=h1", "--experiment", "host=h2"],
    );
    assert!(state.captures.get_by_id("experiment").is_some());
    let routes: Vec<String> = state
        .sections
        .read()
        .sections()
        .iter()
        .map(|s| s.route.clone())
        .collect();
    assert!(!routes.iter().any(|r| r.contains('@')), "{routes:?}");
}

/// A Rezolus agent and a service of one run: the built-in sections once,
/// as today, restricted to the agent; the service's section from the
/// template in its recording's metadata, restricted to the service; the
/// Overview's throughput is the service's.
#[test]
fn a_service_recording_renders_its_own_template() {
    let dir = tempfile::tempdir().unwrap();
    let state = view(&agent_and_service(dir.path()), &[]);
    let routes: Vec<(String, Option<String>)> = state
        .sections
        .read()
        .sections()
        .iter()
        .map(|s| (s.route.clone(), s.instance.clone()))
        .collect();
    assert_eq!(routes[0], ("/overview".to_string(), None));
    assert_eq!(routes[1], ("/service/svc".to_string(), None));
    assert!(routes.iter().all(|(r, i)| !r.contains('@') && i.is_none()));

    let svc = section(&state, "/service/svc");
    let plots = plot_queries(&svc);
    assert_eq!(
        plots[0].1, "sum(irate(svc_requests{source=\"svc\"}[5m]))",
        "{plots:?}"
    );
    let values = last_values(state.baseline_data().as_ref(), &plots[0].1);
    assert_eq!(values.len(), 1);
    assert!((values[0] - 40.0).abs() < 1e-9, "{values:?}");

    let cpu = section(&state, "/cpu");
    for (id, query) in plot_queries(&cpu) {
        assert!(query.contains("source=\"rezolus\""), "{id}: {query}");
    }
    assert_eq!(
        state.sections.read().context().throughput_query.as_deref(),
        Some("sum(irate(svc_requests{source=\"svc\"}[5m]))")
    );
}

/// The WASM viewer opens the same archives as one combined capture and
/// serves the same navigation and the same sections, byte for byte, apart
/// from the `filename` and `filesize` a server reads from the path.
#[test]
fn the_wasm_viewer_renders_a_combined_archive_as_the_server_does() {
    let dir = tempfile::tempdir().unwrap();
    for path in [two_hosts(dir.path()), agent_and_service(dir.path())] {
        let state = view(&path, &[]);
        let mut reg = viewer::WasmCaptureRegistry::new();
        reg.attach("baseline", &std::fs::read(&path).unwrap(), "run.dendro")
            .unwrap();
        reg.init_templates("baseline", "[]").unwrap();
        assert!(!reg.has("experiment"));

        let server_sections = state.sections_metadata()["sections"].clone();
        let wasm_sections: serde_json::Value =
            serde_json::from_str(&reg.get_sections("baseline").unwrap()).unwrap();
        assert_eq!(
            serde_json::to_string(&wasm_sections).unwrap(),
            serde_json::to_string(&server_sections).unwrap(),
            "{}",
            path.display()
        );
        for s in server_sections.as_array().unwrap() {
            let route = s["route"].as_str().unwrap();
            if route == "/query" || route.starts_with("/source/") {
                continue;
            }
            let mut server = section(&state, route);
            let mut wasm: serde_json::Value =
                serde_json::from_str(&reg.get_section("baseline", &route[1..]).unwrap()).unwrap();
            for v in [&mut server, &mut wasm] {
                let o = v.as_object_mut().unwrap();
                o.remove("filename");
                o.remove("filesize");
            }
            assert_eq!(
                serde_json::to_string(&wasm).unwrap(),
                serde_json::to_string(&server).unwrap(),
                "{} {route}",
                path.display()
            );
        }
    }
}

/// Two recordings whose label sets are equal are told apart by
/// `__recording__`, which their instances' queries match on.
#[test]
fn recordings_with_equal_labels_are_kept_apart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("twins.dendro");
    let twin = |cpus, busy_ns| Recording {
        labels: &[("source", "rezolus")],
        metadata: &[("source", "rezolus")],
        cpus,
        busy_ns,
        counter: None,
    };
    one_run(&path, &[twin(1, 250_000_000), twin(1, 750_000_000)]);
    let state = view(&path, &[]);
    let routes: Vec<(String, Option<String>)> = state
        .sections
        .read()
        .sections()
        .iter()
        .map(|s| (s.route.clone(), s.instance.clone()))
        .collect();
    assert!(
        routes.contains(&(
            "/cpu@recording-2".to_string(),
            Some("recording 2".to_string())
        )),
        "{routes:?}"
    );
    let data = state.baseline_data();
    for (route, busy) in [("/cpu", 0.25), ("/cpu@recording-2", 0.75)] {
        let cpu = section(&state, route);
        let (_, query) = plot_queries(&cpu)
            .into_iter()
            .find(|(id, _)| id == "busy-pct")
            .unwrap();
        assert!(query.contains("__recording__"), "{query}");
        let values = last_values(data.as_ref(), &query);
        assert_eq!(values.len(), 1, "{route}: {query} -> {values:?}");
        assert!((values[0] - busy).abs() < 1e-9, "{route}: {values:?}");
    }
}

/// An instance's catalog is its recording's: a metric only another host
/// recorded is not among its names, so a section gated on a metric being
/// present is drawn only for the host that has it.
#[test]
fn an_instance_lists_only_its_own_metrics() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.dendro");
    one_run(
        &path,
        &[
            Recording {
                labels: &[("source", "rezolus"), ("host", "h1")],
                metadata: &[("source", "rezolus")],
                cpus: 1,
                busy_ns: 1,
                counter: None,
            },
            Recording {
                labels: &[("source", "rezolus"), ("host", "h2")],
                metadata: &[("source", "rezolus")],
                cpus: 1,
                busy_ns: 1,
                counter: Some(("cpu_instructions", 5)),
            },
        ],
    );
    let state = view(&path, &[]);
    let data = state.baseline_data();
    let sections = state.sections.read();
    let names = |route: &str| {
        let scope = &sections.context().scopes[route].scope;
        ::dashboard::combined::Scoped::new(data.as_ref(), scope).counter_names()
    };
    assert!(!names("/cpu").contains(&"cpu_instructions".to_string()));
    assert!(names("/cpu@h2").contains(&"cpu_instructions".to_string()));
    assert!(data
        .counter_names()
        .contains(&"cpu_instructions".to_string()));
}

/// Two agents and a service on the first agent's host: the first host's
/// Overview shows the service's throughput, the second host's none (rather
/// than the other host's service), and System Info lists each host's
/// hardware under its name, in both viewers.
#[test]
fn each_instance_has_its_own_throughput_and_systeminfo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.dendro");
    let template = svc_template();
    one_run(
        &path,
        &[
            Recording {
                labels: &[("source", "rezolus"), ("host", "h1")],
                metadata: &[
                    ("source", "rezolus"),
                    ("systeminfo", r#"{"hostname":"h1","cpus":[]}"#),
                ],
                cpus: 1,
                busy_ns: 1,
                counter: None,
            },
            Recording {
                labels: &[("source", "rezolus"), ("host", "h2")],
                metadata: &[
                    ("source", "rezolus"),
                    ("systeminfo", r#"{"hostname":"h2","cpus":[]}"#),
                ],
                cpus: 1,
                busy_ns: 1,
                counter: None,
            },
            Recording {
                labels: &[("source", "svc"), ("host", "h1")],
                metadata: &[("source", "svc"), ("service_queries", &template)],
                cpus: 0,
                busy_ns: 0,
                counter: Some(("svc_requests", 40)),
            },
        ],
    );
    let state = view(&path, &[]);
    {
        let sections = state.sections.read();
        let scopes = &sections.context().scopes;
        assert_eq!(
            scopes["/overview"].throughput.as_deref(),
            Some(r#"sum(irate(svc_requests{host="h1",source="svc"}[5m]))"#)
        );
        assert_eq!(scopes["/overview@h2"].throughput, None);
    }
    let info: serde_json::Value =
        serde_json::from_str(&state.captures.systeminfo_by_id("baseline").unwrap()).unwrap();
    assert_eq!(info["h1"]["hostname"], "h1");
    assert_eq!(info["h2"]["hostname"], "h2");

    let mut reg = viewer::WasmCaptureRegistry::new();
    reg.attach("baseline", &std::fs::read(&path).unwrap(), "run.dendro")
        .unwrap();
    let wasm: serde_json::Value =
        serde_json::from_str(&reg.systeminfo("baseline").unwrap()).unwrap();
    assert_eq!(wasm, info);
}

/// A recording that lacks a label another recording carries is selected
/// by `role!="x"`, and the engine answers each instance from its own
/// recording only.
#[test]
fn a_recording_without_the_others_label_reads_only_its_series() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.dendro");
    one_run(
        &path,
        &[
            Recording {
                labels: &[("source", "rezolus"), ("host", "a")],
                metadata: &[("source", "rezolus")],
                cpus: 1,
                busy_ns: 250_000_000,
                counter: None,
            },
            Recording {
                labels: &[("source", "rezolus"), ("host", "a"), ("role", "x")],
                metadata: &[("source", "rezolus")],
                cpus: 2,
                busy_ns: 750_000_000,
                counter: None,
            },
        ],
    );
    let state = view(&path, &[]);
    let routes: Vec<String> = state
        .sections
        .read()
        .sections()
        .iter()
        .map(|s| s.route.clone())
        .collect();
    assert!(routes.contains(&"/cpu@x".to_string()), "{routes:?}");
    let data = state.baseline_data();
    for (route, busy, per_cpu) in [("/cpu", 0.25, false), ("/cpu@x", 0.75, true)] {
        let plots = plot_queries(&section(&state, route));
        assert_eq!(
            plots.iter().any(|(id, _)| id == "busy-pct-per-cpu"),
            per_cpu,
            "{route}: charts from its own catalog"
        );
        let (_, query) = plots.into_iter().find(|(id, _)| id == "busy-pct").unwrap();
        let values = last_values(data.as_ref(), &query);
        assert_eq!(values.len(), 1, "{route}: {query} -> {values:?}");
        assert!(
            (values[0] - busy).abs() < 1e-9,
            "{route}: {query} {values:?}"
        );
    }
}

/// With two other values of the label, the recording lacking it is
/// selected by `role!~"x|y"`.
#[test]
fn a_recording_without_a_label_two_others_carry_reads_only_its_series() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.dendro");
    let rec = |labels, busy_ns| Recording {
        labels,
        metadata: &[("source", "rezolus")],
        cpus: 1,
        busy_ns,
        counter: None,
    };
    one_run(
        &path,
        &[
            rec(&[("source", "rezolus")], 100_000_000),
            rec(&[("source", "rezolus"), ("role", "x")], 200_000_000),
            rec(&[("source", "rezolus"), ("role", "y")], 300_000_000),
        ],
    );
    let state = view(&path, &[]);
    let data = state.baseline_data();
    let (_, query) = plot_queries(&section(&state, "/cpu"))
        .into_iter()
        .find(|(id, _)| id == "busy-pct")
        .unwrap();
    assert!(query.contains(r#"role!~"x|y""#), "{query}");
    let values = last_values(data.as_ref(), &query);
    assert_eq!(values.len(), 1, "{query} -> {values:?}");
    assert!((values[0] - 0.1).abs() < 1e-9, "{values:?}");
}

/// An archive whose arms are in reverse order, and one whose arms cannot
/// fill two slots.
fn armed(dir: &Path, name: &str, arms: &[&str]) -> PathBuf {
    let path = dir.join(name);
    let labels: Vec<std::collections::BTreeMap<String, String>> = arms
        .iter()
        .enumerate()
        .map(|(i, arm)| {
            [
                ("source".to_string(), format!("s{i}")),
                ("host".to_string(), "web-01".to_string()),
                ("arm".to_string(), arm.to_string()),
            ]
            .into()
        })
        .collect();
    crate::mcp::tests::multi_recording_with_labels(&path, &labels);
    path
}

/// The baseline slot holds the `arm=baseline` recording whatever the
/// archive's order, in both viewers; arms that cannot fill two slots are
/// refused by both.
#[test]
fn the_arms_fill_the_slots_in_both_viewers() {
    let dir = tempfile::tempdir().unwrap();
    let reversed = armed(dir.path(), "rev.dendro", &["experiment", "baseline"]);
    let state = view(&reversed, &[]);
    let meta = |id: &str| state.captures.file_metadata_by_id(id).unwrap();
    assert!(meta("baseline").contains("s1"), "{}", meta("baseline"));
    assert!(meta("experiment").contains("s0"), "{}", meta("experiment"));

    let mut reg = viewer::WasmCaptureRegistry::new();
    reg.attach("baseline", &std::fs::read(&reversed).unwrap(), "rev.dendro")
        .unwrap();
    assert!(reg.file_metadata_json("baseline").unwrap().contains("s1"));
    assert!(reg.file_metadata_json("experiment").unwrap().contains("s0"));

    let shared = armed(dir.path(), "shared.dendro", &["baseline", "baseline"]);
    let labels: Vec<_> = crate::rez_reader::RezReader::open_recordings(
        &shared,
        metriken_storage::BufferPool::new(1 << 24),
    )
    .unwrap()
    .into_iter()
    .map(|(l, _)| l)
    .collect();
    let none = crate::mcp::RecordingSelector::default();
    let err = super::resolve_ab_slots(&labels, &none, &none).unwrap_err();
    assert!(
        err.contains("same `arm`") && err.contains("--baseline"),
        "{err}"
    );
    // The WASM viewer's refusal is `comparison_order`'s (crates/viewer):
    // its JsValue error aborts outside wasm32.
}

/// The experiment's own events in [`concurrent_ab`].
pub(crate) const SVC_B_EVENTS: &str =
    r#"{"events":[{"timestamp":1700000001000000000,"kind":"run_start","description":"bench"}]}"#;

/// The owner's concurrent A/B: two services with `arm=`, each carrying
/// [`svc_template`], and a Rezolus agent without one on the same host (with
/// systeminfo), recorded in one run.
pub(crate) fn concurrent_ab(dir: &Path) -> PathBuf {
    let path = dir.join("conc.dendro");
    let template = svc_template();
    one_run(
        &path,
        &[
            Recording {
                labels: &[("source", "rezolus"), ("host", "h")],
                metadata: &[
                    ("source", "rezolus"),
                    ("systeminfo", r#"{"hostname":"h","cpus":[]}"#),
                ],
                cpus: 2,
                busy_ns: 500_000_000,
                counter: None,
            },
            Recording {
                labels: &[("source", "svcB"), ("host", "h"), ("arm", "experiment")],
                metadata: &[
                    ("source", "svcB"),
                    ("service_queries", &template),
                    // Its own events (`record -- cmd`), not a report's.
                    ("events", SVC_B_EVENTS),
                ],
                cpus: 0,
                busy_ns: 0,
                counter: Some(("svc_requests", 20)),
            },
            Recording {
                labels: &[("source", "svcA"), ("host", "h"), ("arm", "baseline")],
                metadata: &[("source", "svcA"), ("service_queries", &template)],
                cpus: 0,
                busy_ns: 0,
                counter: Some(("svc_requests", 10)),
            },
        ],
    );
    path
}

/// The arms fill the comparison's two slots, and the agent is shown beside
/// it, not as a third arm: its sections at `@h`, grouped under its name,
/// rendering as a single capture, every query naming it by `__capture__`,
/// which the baseline's source answers from the agent alone. The same in
/// both viewers, from file mode and from an upload.
#[test]
fn a_concurrent_ab_shows_the_unarmed_agent_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = concurrent_ab(dir.path());
    let state = view(&path, &[]);
    let meta = |id: &str| state.captures.file_metadata_by_id(id).unwrap();
    assert!(meta("baseline").contains("svcA"), "{}", meta("baseline"));
    assert!(
        meta("experiment").contains("svcB"),
        "{}",
        meta("experiment")
    );
    assert_eq!(state.captures.capture_ids(), ["baseline", "experiment"]);

    let sections: Vec<(String, Option<String>)> = state
        .sections
        .read()
        .sections()
        .iter()
        .map(|s| (s.route.clone(), s.instance.clone()))
        .collect();
    assert!(
        sections.contains(&("/cpu@h".to_string(), Some("h".to_string()))),
        "{sections:?}"
    );
    assert!(
        sections
            .iter()
            .filter(|(r, _)| !r.contains('@'))
            .all(|(_, i)| i.is_none()),
        "the comparison's sections keep their routes: {sections:?}"
    );
    // A beside recording gets no query explorer or `/source/` entry of its
    // own.
    assert!(
        !sections.iter().any(|(r, _)| r.starts_with("/query@")),
        "{sections:?}"
    );
    // The baseline (svcA) has no system metrics: the page opens on the
    // comparison's first service section.
    let landing: Vec<String> = state
        .sections
        .read()
        .sections()
        .iter()
        .filter(|s| s.landing)
        .map(|s| s.route.clone())
        .collect();
    assert_eq!(landing, ["/service/svcA"]);
    // The agent's system info and metadata are listed with the baseline's.
    let info: serde_json::Value =
        serde_json::from_str(&state.captures.systeminfo_by_id("baseline").unwrap()).unwrap();
    assert_eq!(
        info,
        serde_json::json!({"h": {"hostname": "h", "cpus": []}})
    );
    let file_meta: serde_json::Value = serde_json::from_str(&meta("baseline")).unwrap();
    assert_eq!(file_meta["beside_recordings"]["h"]["source"], "rezolus");
    let cpu = section(&state, "/cpu@h");
    assert_eq!(cpu["metadata"]["single_capture"], "h");
    let (_, busy) = plot_queries(&cpu)
        .into_iter()
        .find(|(id, _)| id == "busy-pct")
        .unwrap();
    assert!(busy.contains(r#"__capture__="h""#), "{busy}");
    let values = last_values(state.baseline_data().as_ref(), &busy);
    assert_eq!(values.len(), 1, "{busy}: {values:?}");
    assert!((values[0] - 0.5).abs() < 1e-9, "{values:?}");
    // The comparison still compares: the baseline's own query reads svcA.
    let svc = last_values(
        state.baseline_data().as_ref(),
        "sum(irate(svc_requests[5m]))",
    );
    assert_eq!(svc.len(), 1);
    assert!((svc[0] - 10.0).abs() < 1e-9, "{svc:?}");

    let mut reg = viewer::WasmCaptureRegistry::new();
    reg.attach("baseline", &std::fs::read(&path).unwrap(), "conc.dendro")
        .unwrap();
    reg.init_templates("baseline", "[]").unwrap();
    // Before the combined regeneration the baseline's own sections already
    // carry the agent's.
    let first: Vec<serde_json::Value> =
        serde_json::from_str(&reg.get_sections("baseline").unwrap()).unwrap();
    assert!(first.iter().any(|s| s["route"] == "/cpu@h"), "{first:?}");
    reg.init_templates("experiment", "[]").unwrap();
    reg.regenerate_combined("[]", None).unwrap();
    assert!(reg.file_metadata_json("baseline").unwrap().contains("svcA"));
    let ids: Vec<serde_json::Value> = serde_json::from_str(&reg.captures()).unwrap();
    assert_eq!(ids.len(), 2);
    let wasm_sections: Vec<serde_json::Value> =
        serde_json::from_str(&reg.get_sections("baseline").unwrap()).unwrap();
    assert!(wasm_sections
        .iter()
        .any(|s| s["route"] == "/cpu@h" && s["instance"] == "h"));
    let wasm_cpu: serde_json::Value =
        serde_json::from_str(&reg.get_section("baseline", "cpu@h").unwrap()).unwrap();
    assert_eq!(wasm_cpu["metadata"]["single_capture"], "h");
    let wasm_landing: Vec<&serde_json::Value> = wasm_sections
        .iter()
        .filter(|s| s["landing"] == true)
        .map(|s| &s["route"])
        .collect();
    assert_eq!(wasm_landing, ["/service/svcA"]);
    let wasm_info: serde_json::Value =
        serde_json::from_str(&reg.systeminfo("baseline").unwrap()).unwrap();
    assert_eq!(wasm_info, info);
    let wasm_meta: serde_json::Value =
        serde_json::from_str(&reg.file_metadata_json("baseline").unwrap()).unwrap();
    assert_eq!(wasm_meta["beside_recordings"]["h"]["source"], "rezolus");
}

/// Without a service template the comparison has no service section, so a
/// baseline without system metrics opens on the first beside recording's
/// overview; a beside service with no template of its own adds no
/// `/source/` entry.
#[test]
fn a_comparison_without_services_lands_on_the_agent_overview() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.dendro");
    let template: &'static str = Box::leak(svc_template().into_boxed_str());
    let svc_d: &'static [(&'static str, &'static str)] =
        Box::leak(vec![("source", "svcD"), ("service_queries", template)].into_boxed_slice());
    let svc = |labels: &'static [(&'static str, &'static str)],
               metadata: &'static [(&'static str, &'static str)],
               n: u64| Recording {
        labels,
        metadata,
        cpus: 0,
        busy_ns: 0,
        counter: Some(("svc_requests", n)),
    };
    one_run(
        &path,
        &[
            Recording {
                labels: &[("source", "rezolus"), ("host", "h")],
                metadata: &[("source", "rezolus")],
                cpus: 1,
                busy_ns: 500_000_000,
                counter: None,
            },
            svc(
                &[("source", "svcA"), ("host", "h"), ("arm", "baseline")],
                &[("source", "svcA")],
                10,
            ),
            svc(
                &[("source", "svcB"), ("host", "h"), ("arm", "experiment")],
                &[("source", "svcB")],
                20,
            ),
            svc(
                &[("source", "svcC"), ("host", "h")],
                &[("source", "svcC")],
                30,
            ),
            // A beside service with a template has a service section of its
            // own, which is not where the page opens.
            svc(&[("source", "svcD"), ("host", "h")], svc_d, 40),
        ],
    );
    let state = view(&path, &[]);
    let store = state.sections.read();
    let sections = store.sections();
    let routes: Vec<&str> = sections.iter().map(|s| s.route.as_str()).collect();
    assert!(
        !routes
            .iter()
            .any(|r| r.starts_with("/source/") && r.contains('@')),
        "{routes:?}"
    );
    let landing: Vec<&str> = sections
        .iter()
        .filter(|s| s.landing)
        .map(|s| s.route.as_str())
        .collect();
    assert!(
        routes
            .iter()
            .any(|r| r.starts_with("/service/") && r.contains('@')),
        "{routes:?}"
    );
    assert_eq!(landing, ["/overview@rezolus"], "{routes:?}");
}

/// `--baseline`/`--experiment` choose the slots; every other recording is
/// beside, the agent of the concurrent A/B included.
#[test]
fn selectors_keep_the_other_recordings_beside() {
    let dir = tempfile::tempdir().unwrap();
    let path = concurrent_ab(dir.path());
    let state = view(
        &path,
        &["--baseline", "source=svcA", "--experiment", "source=svcB"],
    );
    assert_eq!(state.captures.capture_ids(), ["baseline", "experiment"]);
    let routes: Vec<String> = state
        .sections
        .read()
        .sections()
        .iter()
        .map(|s| s.route.clone())
        .collect();
    assert!(routes.contains(&"/cpu@h".to_string()), "{routes:?}");
}

/// One armed recording among unarmed ones names no comparison: refused by
/// the server's slot resolution and by the WASM viewer's order (the upload
/// path shares the first).
#[test]
fn one_arm_among_unarmed_recordings_is_refused() {
    let labels: Vec<std::collections::BTreeMap<String, String>> = vec![
        [("source".to_string(), "a".to_string())].into(),
        [
            ("source".to_string(), "b".to_string()),
            ("arm".to_string(), "baseline".to_string()),
        ]
        .into(),
    ];
    let none = crate::mcp::RecordingSelector::default();
    let err = super::resolve_ab_slots(&labels, &none, &none).unwrap_err();
    assert!(err.contains("one recording carries an `arm`"), "{err}");
}

/// Agents compared, a third beside: the baseline has system metrics, so the
/// page keeps its usual landing, in both viewers.
#[test]
fn a_comparison_of_agents_keeps_the_usual_landing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agents.dendro");
    let agent = |labels: &'static [(&'static str, &'static str)]| Recording {
        labels,
        metadata: &[("source", "rezolus")],
        cpus: 1,
        busy_ns: 250_000_000,
        counter: None,
    };
    one_run(
        &path,
        &[
            agent(&[("source", "rezolus"), ("host", "h1"), ("arm", "baseline")]),
            agent(&[("source", "rezolus"), ("host", "h2"), ("arm", "experiment")]),
            agent(&[("source", "rezolus"), ("host", "h3")]),
        ],
    );
    let state = view(&path, &[]);
    let store = state.sections.read();
    let sections = store.sections();
    assert!(sections.iter().any(|s| s.route == "/cpu@h3"));
    assert!(!sections.iter().any(|s| s.landing));
    drop(store);

    let mut reg = viewer::WasmCaptureRegistry::new();
    reg.attach("baseline", &std::fs::read(&path).unwrap(), "agents.dendro")
        .unwrap();
    reg.init_templates("baseline", "[]").unwrap();
    let wasm: Vec<serde_json::Value> =
        serde_json::from_str(&reg.get_sections("baseline").unwrap()).unwrap();
    assert!(wasm.iter().any(|s| s["route"] == "/cpu@h3"));
    assert!(!wasm.iter().any(|s| s.get("landing").is_some()));
}

/// What the template lookup says about a beside recording reaches the
/// page's notices from the combined regeneration too.
#[test]
fn a_beside_recordings_template_notice_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.dendro");
    let svc = |labels: &'static [(&'static str, &'static str)],
               metadata: &'static [(&'static str, &'static str)]| Recording {
        labels,
        metadata,
        cpus: 0,
        busy_ns: 0,
        counter: Some(("svc_requests", 10)),
    };
    one_run(
        &path,
        &[
            Recording {
                labels: &[("source", "rezolus"), ("host", "h")],
                metadata: &[
                    ("source", "rezolus"),
                    ("service_queries", "{not json"),
                    // A section template, which the baseline's metadata does
                    // not repeat for a beside recording.
                    ("dashboard", "{not json either"),
                ],
                cpus: 1,
                busy_ns: 250_000_000,
                counter: None,
            },
            svc(
                &[("source", "svcA"), ("host", "h"), ("arm", "baseline")],
                &[("source", "svcA")],
            ),
            svc(
                &[("source", "svcB"), ("host", "h"), ("arm", "experiment")],
                &[("source", "svcB")],
            ),
        ],
    );
    let mut reg = viewer::WasmCaptureRegistry::new();
    reg.attach("baseline", &std::fs::read(&path).unwrap(), "bad.dendro")
        .unwrap();
    reg.init_templates("baseline", "[]").unwrap();
    reg.init_templates("experiment", "[]").unwrap();
    let before: Vec<String> = serde_json::from_str(&reg.notices()).unwrap();
    reg.regenerate_combined("[]", None).unwrap();
    let after: Vec<String> = serde_json::from_str(&reg.notices()).unwrap();
    assert!(after.len() > before.len(), "{before:?} -> {after:?}");
    let wasm: serde_json::Value =
        serde_json::from_str(&reg.file_metadata_json("baseline").unwrap()).unwrap();
    let state = view(&path, &[]);
    let server: serde_json::Value =
        serde_json::from_str(&state.captures.file_metadata_by_id("baseline").unwrap()).unwrap();
    for meta in [wasm, server] {
        let agent = &meta["beside_recordings"]["h"];
        assert_eq!(agent["source"], "rezolus", "{meta}");
        assert!(agent.get("dashboard").is_none(), "{meta}");
    }
}

/// A report saved from the concurrent A/B (the agent first in the archive,
/// unarmed) lands on the baseline's recording, svcA's, and a fresh viewer
/// of it opens in report mode with the pins, in both viewers
/// (`report_save`'s dendro path). A report saved before that, onto the
/// archive's first recording, reopens the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_report_of_the_concurrent_ab_reopens_with_its_pins() {
    let dir = tempfile::tempdir().unwrap();
    let dendro = concurrent_ab(dir.path());
    report_round_trip(dir.path(), &dendro).await;
}

async fn report_round_trip(dir: &Path, path: &Path) {
    let ext = "dendro";
    use crate::parquet_metadata::{KEY_EVENTS, KEY_REPORT, KEY_SELECTION};
    let body = r#"{"entries":[{"chartId":"busy-pct","section":"cpu@h","promql_query":"irate(cpu_usage{__capture__=\"h\"}[5m])","single_capture":true}],"trim_columns":true}"#;

    // What a reload finds: selection and report mode, server and WASM.
    let reopen = |name: &str, bytes: &[u8]| {
        let name = format!("{name}.{ext}");
        let report = dir.join(&name);
        std::fs::write(&report, bytes).unwrap();
        let state = view(&report, &[]);
        assert_eq!(state.selection.read().as_deref(), Some(body), "{name}");
        assert_eq!(
            state.trimmed_report_marker.read().as_deref(),
            Some("trimmed"),
            "{name}"
        );
        // The report keeps only the agent's pinned columns, so svcA holds
        // no rows: the page's span is the agent's.
        assert!(state.baseline_data().time_range().is_some(), "{name}");
        let mut reg = viewer::WasmCaptureRegistry::new();
        reg.attach("baseline", bytes, &name).unwrap();
        assert_eq!(reg.selection("baseline").as_deref(), Some(body), "{name}");
        let info: serde_json::Value = serde_json::from_str(&reg.info("baseline").unwrap()).unwrap();
        assert!(info["maxTime"].as_f64().unwrap() > 0.0, "{name}: {info}");
        let meta: serde_json::Value =
            serde_json::from_str(&reg.file_metadata_json("baseline").unwrap()).unwrap();
        assert_eq!(meta[KEY_REPORT], "trimmed", "{name}");
    };
    // The experiment's own events, in a report: kept by every save.
    let svc_b_events = |bytes: &[u8]| -> Option<String> {
        let archive = dendro::archive::Archive::open_bytes(bytes.to_vec()).unwrap();
        let sources = archive.read_sources().unwrap();
        let svc = sources.iter().find(|s| s.meta.labels["source"] == "svcB")?;
        svc.meta.metadata.get(KEY_EVENTS).cloned()
    };
    // The `source` of each recording a report's markers are on.
    let marked = |bytes: &[u8]| -> Vec<String> {
        let archive = dendro::archive::Archive::open_bytes(bytes.to_vec()).unwrap();
        archive
            .read_sources()
            .unwrap()
            .into_iter()
            .filter(|s| s.meta.metadata.contains_key(KEY_SELECTION))
            .map(|s| s.meta.labels["source"].clone())
            .collect()
    };

    let state = std::sync::Arc::new(view(path, &[]));
    let response = super::actions::save_with_selection(
        axum::extract::State(std::sync::Arc::clone(&state)),
        body.to_string(),
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let server = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(marked(&server), ["svcA"], "{ext}");
    assert_eq!(
        svc_b_events(&server).as_deref(),
        Some(SVC_B_EVENTS),
        "{ext}"
    );
    reopen("server", &server);

    let mut reg = viewer::WasmCaptureRegistry::new();
    let name = path.file_name().unwrap().to_str().unwrap();
    reg.attach("baseline", &std::fs::read(path).unwrap(), name)
        .unwrap();
    let wasm = reg.save_with_selection(body).unwrap();
    assert_eq!(marked(&wasm), ["svcA"], "{ext}");
    assert_eq!(svc_b_events(&wasm).as_deref(), Some(SVC_B_EVENTS), "{ext}");
    reopen("wasm", &wasm);

    // Saved onto the archive's first recording, as before the fix.
    let keep: std::collections::BTreeSet<String> = ["cpu_usage".to_string()].into_iter().collect();
    let old = ::report_save::build_report(
        ::report_save::ReportSource::Dendro(&std::fs::read(path).unwrap()),
        None,
        Some(&keep),
        None,
        body,
        Some(r#"{"events":[{"timestamp":1700000002000000000,"description":"old"}]}"#),
    )
    .unwrap();
    assert_eq!(marked(&old), ["rezolus"], "{ext}");
    reopen("old", &old);
    // The old report's events, on the agent beside the comparison, reach
    // the baseline's metadata, where the page reads them.
    let old_report = dir.join(format!("old-events.{ext}"));
    std::fs::write(&old_report, &old).unwrap();
    let meta: serde_json::Value = serde_json::from_str(
        &view(&old_report, &[])
            .captures
            .file_metadata_by_id("baseline")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        meta["events"]["events"][0]["description"], "old",
        "{ext}: {meta}"
    );

    // Saved again from that report: only svcA carries a report.
    let old_path = dir.join(format!("old-source.{ext}"));
    std::fs::write(&old_path, &old).unwrap();
    let state = std::sync::Arc::new(view(&old_path, &[]));
    let response = super::actions::save_with_selection(
        axum::extract::State(std::sync::Arc::clone(&state)),
        body.to_string(),
    )
    .await;
    let again = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(marked(&again), ["svcA"], "{ext}");
    assert_eq!(svc_b_events(&again).as_deref(), Some(SVC_B_EVENTS), "{ext}");
}

/// Agents compared with the `cpu_usage` sampler off: the baseline is still
/// an agent, so the page keeps its own overview rather than a beside one.
#[test]
fn an_agent_baseline_without_cpu_usage_keeps_the_usual_landing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nocpu.dendro");
    let agent = |labels: &'static [(&'static str, &'static str)]| Recording {
        labels,
        metadata: &[("source", "rezolus")],
        cpus: 0,
        busy_ns: 0,
        counter: Some(("memory_total", 1)),
    };
    one_run(
        &path,
        &[
            agent(&[("source", "rezolus"), ("host", "h1"), ("arm", "baseline")]),
            agent(&[("source", "rezolus"), ("host", "h2"), ("arm", "experiment")]),
            agent(&[("source", "rezolus"), ("host", "h3")]),
        ],
    );
    let state = view(&path, &[]);
    assert!(!state
        .baseline_data()
        .counter_names()
        .contains(&"cpu_usage".to_string()));
    let store = state.sections.read();
    let sections = store.sections();
    assert!(sections.iter().any(|s| s.route == "/overview@h3"));
    assert!(!sections.iter().any(|s| s.landing));
}

/// A combined context built with the archive as its scopes' catalog opens
/// no table, and rendering one recording's section opens only that
/// recording's tables. Built without it, the context opens none either
/// here: the fixture is written by metriken-storage 0.6, whose segments carry
/// a names index the labels are listed from. An archive `record` writes
/// (metriken-recorder 0.5.1, over storage 0.5.4) has no such index, and on
/// that path opens a table of each recording of more than one segment.
#[test]
fn a_recordings_catalog_opens_only_its_own_tables() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hosts.dendro");
    let agent = |labels: &'static [(&'static str, &'static str)]| Recording {
        labels,
        metadata: &[("source", "rezolus")],
        cpus: 2,
        busy_ns: 250_000_000,
        counter: None,
    };
    one_run(
        &path,
        &[
            agent(&[("source", "rezolus"), ("host", "h1")]),
            agent(&[("source", "rezolus"), ("host", "h2")]),
            agent(&[("source", "rezolus"), ("host", "h3")]),
        ],
    );
    let open = || {
        let pool = metriken_storage::BufferPool::new(64 << 20);
        let readers = crate::rez_reader::RezReader::open_recordings(&path, pool).unwrap();
        let combined = super::combine_recordings(readers);
        (Arc::new(combined.reader), combined.members)
    };
    let registry = dashboard::TemplateRegistry::empty();
    let pref = Default::default();
    let tables = |r: &crate::rez_reader::RezReader| r.owners_of(&r.all_names()).len();

    let (reader, members) = open();
    let per_recording = tables(&reader) / 3;
    let catalog: Arc<dyn dashboard::combined::ScopeCatalog> =
        Arc::new(dashboard::combined::FixedCatalog(Arc::clone(&reader)));
    let (ctx, _) = dashboard::dashboard::build_combined_context(
        reader.as_ref(),
        Some(catalog),
        &members,
        &registry,
        pref,
        None,
        None,
    );
    assert!(
        reader.opened_tables().is_empty(),
        "{:?}",
        reader.opened_tables()
    );
    let cpu = dashboard::dashboard::render_section(reader.as_ref(), "/cpu@h2", &ctx).unwrap();
    let view = serde_json::to_value(&cpu.view).unwrap();
    assert!(!view["groups"].as_array().unwrap().is_empty(), "{view}");
    let opened = reader.opened_tables().len();
    assert!(
        (1..=per_recording).contains(&opened),
        "{opened} tables opened, {per_recording} per recording"
    );

    let (reader, members) = open();
    let _ = dashboard::dashboard::build_combined_context(
        reader.as_ref(),
        None,
        &members,
        &registry,
        pref,
        None,
        None,
    );
    assert!(
        reader.opened_tables().is_empty(),
        "{:?}",
        reader.opened_tables()
    );
}

/// A source of [`dendro_of`]: its labels, and the snapshot it records at a
/// tick.
type FixtureSource<'a> = (
    &'a [(&'a str, &'a str)],
    &'a dyn Fn(u64) -> metriken_exposition::Snapshot,
);

/// A dendro archive of `sources`, each a label set and the snapshot it
/// records at tick `i`, three ticks each.
fn dendro_of(path: &Path, sources: &[FixtureSource<'_>]) {
    use crate::dendro_copy::fixtures::{ANCHOR, SECOND};
    use metriken_storage::{ArchiveWriter, WriterConfig};
    let mut writer = ArchiveWriter::create(path, WriterConfig::default()).unwrap();
    let mut recs: Vec<_> = sources
        .iter()
        .map(|(labels, _)| {
            let labels = labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            writer
                .add_source(labels, Default::default(), ANCHOR)
                .unwrap()
        })
        .collect();
    for i in 0..3u64 {
        let staged = recs
            .iter_mut()
            .zip(sources)
            .map(|(r, (_, snap))| r.stage(&snap(i), ANCHOR + i * SECOND, 0).unwrap())
            .collect();
        writer.commit(staged).unwrap();
    }
    for r in recs {
        r.finalize((ANCHOR + 2 * SECOND, 0)).unwrap();
    }
    writer.join().unwrap();
}

/// The kind a [`dual`] snapshot holds `dual` under.
#[derive(Clone, Copy, PartialEq)]
enum Held {
    Counter,
    Gauge,
    Histogram,
}

/// One group `g/x` holding `dual` as a counter, a gauge or a histogram.
fn dual(i: u64, kind: Held) -> metriken_exposition::Snapshot {
    use crate::dendro_copy::fixtures::{ANCHOR, SECOND};
    use metriken_exposition::{GroupSchema, GroupSnapshot, MetricDesc, Snapshot, SnapshotV3};
    let ts = ANCHOR + i * SECOND;
    let desc = MetricDesc {
        name: "0".to_string(),
        metadata: [("metric".to_string(), "dual".to_string())].into(),
    };
    let only = |k: Held| {
        if kind == k {
            vec![desc.clone()]
        } else {
            Vec::new()
        }
    };
    let schema = GroupSchema {
        counters: only(Held::Counter),
        gauges: only(Held::Gauge),
        histograms: only(Held::Histogram),
        gauge_histograms: Vec::new(),
    };
    let mut histogram = ::histogram::Histogram::new(7, 64).unwrap();
    histogram.increment(1_000 + i).unwrap();
    Snapshot::V3(SnapshotV3 {
        systemtime: std::time::UNIX_EPOCH + std::time::Duration::from_nanos(ts),
        duration: std::time::Duration::ZERO,
        metadata: Default::default(),
        groups: vec![GroupSnapshot {
            name: "g/x".to_string(),
            schema_hash: schema.hash(),
            schema: (i == 0).then(|| std::sync::Arc::new(schema)),
            window: Some(metriken::Window::new(ts - SECOND / 2, ts)),
            counters: match kind {
                Held::Counter => vec![Some(i)],
                _ => Vec::new(),
            },
            gauges: match kind {
                Held::Gauge => vec![Some(i as i64)],
                _ => Vec::new(),
            },
            histograms: match kind {
                Held::Histogram => vec![Some(histogram)],
                _ => Vec::new(),
            },
            gauge_histograms: Vec::new(),
        }],
    })
}

/// Each recording's names and label sets under the archive's catalog
/// ([`dashboard::combined::ScopeCatalog`]) equal those listed through the
/// source and filtered: a matcher on a key the recording lacks is decided
/// by its series' own labels (`comm`, which the third recording carries
/// natively), and a name held under another kind by another recording is
/// not this recording's.
#[test]
fn the_catalog_answers_as_the_filtered_listing_does() {
    use crate::dendro_copy::fixtures::tick;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.dendro");
    let counter = |i| dual(i, Held::Counter);
    let gauge = |i| dual(i, Held::Gauge);
    let histogram = |i| dual(i, Held::Histogram);
    dendro_of(
        &path,
        &[
            (&[("source", "rezolus"), ("comm", "nginx")], &tick),
            (&[("source", "rezolus"), ("comm", "redis")], &tick),
            (&[("source", "rezolus")], &tick),
            (&[("source", "svc"), ("host", "h1")], &counter),
            (&[("source", "svc"), ("host", "h2")], &gauge),
        ],
    );
    let pool = metriken_storage::BufferPool::new(64 << 20);
    let readers = crate::rez_reader::RezReader::open_recordings(&path, pool).unwrap();
    let combined = super::combine_recordings(readers);
    let reader = Arc::new(combined.reader);
    let catalog: Arc<dyn dashboard::combined::ScopeCatalog> =
        Arc::new(dashboard::combined::FixedCatalog(Arc::clone(&reader)));
    let labels: Vec<_> = combined
        .members
        .iter()
        .map(|m| m.series_labels.clone())
        .collect();
    let mut asked = 0;
    for (member, matchers) in combined
        .members
        .iter()
        .zip(dashboard::combined::scope_matchers(&labels))
    {
        let scope = |catalog| dashboard::combined::Scope {
            matchers: matchers.clone(),
            header: member.header.clone(),
            metadata: member.metadata.clone(),
            catalog,
        };
        let (with, without) = (scope(Some(Arc::clone(&catalog))), scope(None));
        let fast = dashboard::combined::Scoped::new(reader.as_ref(), &with);
        let slow = dashboard::combined::Scoped::new(reader.as_ref(), &without);
        assert_eq!(fast.counter_names(), slow.counter_names(), "{matchers:?}");
        assert_eq!(fast.gauge_names(), slow.gauge_names(), "{matchers:?}");
        for name in ["task_ops", "mem_free", "dual"] {
            assert_eq!(
                fast.counter_labels(name),
                slow.counter_labels(name),
                "{name}"
            );
            assert_eq!(fast.gauge_labels(name), slow.gauge_labels(name), "{name}");
            asked += 1;
        }
    }
    assert_eq!(asked, 15);
    // The third recording lacks `comm`; both of its threads carry one of
    // the others' values natively, so it has no `task_ops` series.
    let third = &combined.members[2];
    let matchers = dashboard::combined::scope_matchers(&labels)[2].clone();
    let scope = dashboard::combined::Scope {
        matchers,
        header: third.header.clone(),
        metadata: third.metadata.clone(),
        catalog: Some(Arc::clone(&catalog)),
    };
    let scoped = dashboard::combined::Scoped::new(reader.as_ref(), &scope);
    assert!(!scoped.counter_names().contains(&"task_ops".to_string()));
    assert!(scoped.counter_labels("task_ops").is_empty());
    assert!(scoped.gauge_names().contains(&"mem_free".to_string()));
    // Of three services, each holds `dual` under one kind only: a counter,
    // a gauge, a histogram (every matcher decided by the recordings'
    // labels).
    let path = dir.path().join("dual.dendro");
    dendro_of(
        &path,
        &[
            (&[("source", "svc"), ("host", "h1")], &counter),
            (&[("source", "svc"), ("host", "h2")], &gauge),
            (&[("source", "svc"), ("host", "h3")], &histogram),
        ],
    );
    let pool = metriken_storage::BufferPool::new(64 << 20);
    let readers = crate::rez_reader::RezReader::open_recordings(&path, pool).unwrap();
    let combined = super::combine_recordings(readers);
    let reader = Arc::new(combined.reader);
    let labels: Vec<_> = combined
        .members
        .iter()
        .map(|m| m.series_labels.clone())
        .collect();
    let h2 = &combined.members[1];
    let scope = dashboard::combined::Scope {
        matchers: dashboard::combined::scope_matchers(&labels)[1].clone(),
        header: h2.header.clone(),
        metadata: h2.metadata.clone(),
        catalog: Some(Arc::new(dashboard::combined::FixedCatalog(Arc::clone(
            &reader,
        )))),
    };
    let scoped = dashboard::combined::Scoped::new(reader.as_ref(), &scope);
    assert_eq!(h2.series_labels["host"], "h2");
    assert!(
        scoped.counter_names().is_empty(),
        "{:?}",
        scoped.counter_names()
    );
    assert_eq!(scoped.gauge_names(), ["dual"]);
    let h1 = &combined.members[0];
    let scope = dashboard::combined::Scope {
        matchers: dashboard::combined::scope_matchers(&labels)[0].clone(),
        header: h1.header.clone(),
        metadata: h1.metadata.clone(),
        catalog: Some(Arc::new(dashboard::combined::FixedCatalog(Arc::clone(
            &reader,
        )))),
    };
    let scoped = dashboard::combined::Scoped::new(reader.as_ref(), &scope);
    assert_eq!(scoped.counter_names(), ["dual"]);
    assert!(
        scoped.gauge_names().is_empty(),
        "{:?}",
        scoped.gauge_names()
    );
    assert!(
        scoped.histogram_names().is_empty(),
        "{:?}",
        scoped.histogram_names()
    );
    // The third holds it as a histogram only.
    let h3 = &combined.members[2];
    assert_eq!(h3.series_labels["host"], "h3");
    let scope = dashboard::combined::Scope {
        matchers: dashboard::combined::scope_matchers(&labels)[2].clone(),
        header: h3.header.clone(),
        metadata: h3.metadata.clone(),
        catalog: Some(Arc::new(dashboard::combined::FixedCatalog(Arc::clone(
            &reader,
        )))),
    };
    let scoped = dashboard::combined::Scoped::new(reader.as_ref(), &scope);
    assert_eq!(scoped.histogram_names(), ["dual"]);
    assert!(
        scoped.counter_names().is_empty(),
        "{:?}",
        scoped.counter_names()
    );
    assert_eq!(scoped.histogram_labels("dual").len(), 1);
}
