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
    let path = dir.join("run.rez");
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
    let path = dir.join("svc.rez");
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
        QueryResult::Matrix { result } => {
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
        reg.attach("baseline", &std::fs::read(&path).unwrap(), "run.rez")
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
    let path = dir.path().join("twins.rez");
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
    let path = dir.path().join("run.rez");
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
    let path = dir.path().join("run.rez");
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
    reg.attach("baseline", &std::fs::read(&path).unwrap(), "run.rez")
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
    let path = dir.path().join("run.rez");
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
    let path = dir.path().join("run.rez");
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
    crate::mcp::tests::multi_recording_rez_with_labels(&path, &labels);
    path
}

/// The baseline slot holds the `arm=baseline` recording whatever the
/// archive's order, in both viewers; arms that cannot fill two slots are
/// refused by both.
#[test]
fn the_arms_fill_the_slots_in_both_viewers() {
    let dir = tempfile::tempdir().unwrap();
    let reversed = armed(dir.path(), "rev.rez", &["experiment", "baseline"]);
    let state = view(&reversed, &[]);
    let meta = |id: &str| state.captures.file_metadata_by_id(id).unwrap();
    assert!(meta("baseline").contains("s1"), "{}", meta("baseline"));
    assert!(meta("experiment").contains("s0"), "{}", meta("experiment"));

    let mut reg = viewer::WasmCaptureRegistry::new();
    reg.attach("baseline", &std::fs::read(&reversed).unwrap(), "rev.rez")
        .unwrap();
    assert!(reg.file_metadata_json("baseline").unwrap().contains("s1"));
    assert!(reg.file_metadata_json("experiment").unwrap().contains("s0"));

    let shared = armed(dir.path(), "shared.rez", &["baseline", "baseline"]);
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
