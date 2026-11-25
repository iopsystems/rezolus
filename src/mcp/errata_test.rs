//! Known errata in `describe-recording` and `describe-metrics`.

use super::*;
use rez::fixtures::{one_run, Recording};

fn archive(dir: &std::path::Path, recordings: &[Recording<'_>]) -> std::path::PathBuf {
    let path = dir.join("run.dendro");
    one_run(&path, recordings);
    path
}

fn host(
    name: &'static str,
    metadata: &'static [(&'static str, &'static str)],
) -> Recording<'static> {
    host_with(name, metadata, "network_bytes")
}

fn host_with(
    name: &'static str,
    metadata: &'static [(&'static str, &'static str)],
    counter: &'static str,
) -> Recording<'static> {
    let labels: &'static [(&'static str, &'static str)] = match name {
        "h1" => &[("source", "rezolus"), ("host", "h1")],
        _ => &[("source", "rezolus"), ("host", "h2")],
    };
    Recording {
        labels,
        metadata,
        cpus: 1,
        busy_ns: 250_000_000,
        counter: Some((counter, 1000)),
    }
}

fn describe(path: &std::path::Path, selector: &[&str]) -> String {
    let sel = RecordingSelector::parse(
        "--recording",
        selector.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )
    .unwrap();
    describe_recording_output(path, &sel, SelectorSyntax::Flag("--recording")).unwrap()
}

fn metrics(path: &std::path::Path) -> String {
    let opened = open_selected_labeled(
        path,
        &RecordingSelector::default(),
        SelectorSyntax::Flag("--recording"),
        true,
    )
    .unwrap();
    describe_metrics::format_metrics_description(
        opened.reader.as_ref(),
        &opened.errata,
        !opened.combined.is_empty(),
    )
}

#[test]
fn an_affected_recording_lists_the_erratum() {
    let dir = tempfile::tempdir().unwrap();
    let path = archive(
        dir.path(),
        &[host("h1", &[("source", "rezolus"), ("version", "5.19.0")])],
    );
    let out = describe(&path, &[]);
    assert!(out.contains("\n\nKnown Errata\n============\n"), "{out}");
    // Only the covered metric the recording holds is named.
    assert!(
        out.contains("- network_bytes: misleading (recorded by an agent before 5.20.1-alpha.3)"),
        "{out}"
    );
    assert!(out.contains("pull/1214"), "{out}");

    let m = metrics(&path);
    let at = m.find("• network_bytes").expect("listed");
    let block = &m[at..m[at..].find("\n\n").map(|e| at + e).unwrap()];
    assert!(
        block.contains("  Erratum: misleading (recorded by an agent before 5.20.1-alpha.3)"),
        "{block}"
    );
    // Only the metric the erratum covers.
    let at = m.find("• cpu_usage").expect("listed");
    let block = &m[at..m[at..].find("\n\n").map(|e| at + e).unwrap()];
    assert!(!block.contains("Erratum"), "{block}");
}

#[test]
fn a_fixed_agent_has_no_errata() {
    let dir = tempfile::tempdir().unwrap();
    let path = archive(
        dir.path(),
        &[host(
            "h1",
            &[("source", "rezolus"), ("version", "5.20.1-alpha.3")],
        )],
    );
    assert!(!describe(&path, &[]).contains("Errata"));
    assert!(!metrics(&path).contains("Erratum"));
}

#[test]
fn a_recording_without_a_version_may_be_affected() {
    let dir = tempfile::tempdir().unwrap();
    let path = archive(dir.path(), &[host("h1", &[("source", "rezolus")])]);
    let out = describe(&path, &[]);
    assert!(
        out.contains("misleading (the recording has no Rezolus agent version; agents before 5.20.1-alpha.3 recorded this)"),
        "{out}"
    );
}

#[test]
fn a_combined_read_names_the_affected_recording() {
    let dir = tempfile::tempdir().unwrap();
    let path = archive(
        dir.path(),
        &[
            host("h1", &[("source", "rezolus"), ("version", "5.19.0")]),
            host("h2", &[("source", "rezolus"), ("version", "6.0.0")]),
        ],
    );
    let out = describe(&path, &[]);
    assert!(out.contains("holds 2 recordings of one run"), "{out}");
    assert!(
        out.contains("- [host=h1, source=rezolus] network_bytes: misleading"),
        "{out}"
    );
    assert!(
        !out.contains("host=h2, source=rezolus] network_bytes"),
        "{out}"
    );

    let m = metrics(&path);
    assert!(
        m.contains("  Erratum [host=h1, source=rezolus]: misleading"),
        "{m}"
    );
    assert!(!m.contains("Erratum [host=h2"), "{m}");

    // Selecting the fixed recording drops the erratum, the affected one keeps it.
    assert!(!describe(&path, &["host=h2"]).contains("Errata"));
    assert!(describe(&path, &["host=h1"]).contains("Known Errata"));
}

#[test]
fn a_combined_read_puts_each_erratum_under_the_metrics_its_recording_holds() {
    let dir = tempfile::tempdir().unwrap();
    let path = archive(
        dir.path(),
        &[
            host_with(
                "h1",
                &[("source", "rezolus"), ("version", "5.19.0")],
                "network_bytes",
            ),
            host_with(
                "h2",
                &[("source", "rezolus"), ("version", "5.19.0")],
                "network_packets",
            ),
        ],
    );
    let m = metrics(&path);
    let block = |name: &str| {
        let at = m.find(&format!("• {name}\n")).expect("listed");
        m[at..m[at..].find("\n\n").map(|e| at + e).unwrap()].to_string()
    };
    let bytes = block("network_bytes");
    assert!(bytes.contains("Erratum [host=h1"), "{bytes}");
    assert!(!bytes.contains("Erratum [host=h2"), "{bytes}");
    let packets = block("network_packets");
    assert!(packets.contains("Erratum [host=h2"), "{packets}");
    assert!(!packets.contains("Erratum [host=h1"), "{packets}");
}

#[test]
fn another_producers_version_is_not_compared() {
    let dir = tempfile::tempdir().unwrap();
    // `source` other than rezolus: `version` is the producer's, not an agent's.
    let path = dir.path().join("svc.dendro");
    one_run(
        &path,
        &[Recording {
            labels: &[("source", "svc")],
            metadata: &[("source", "svc"), ("version", "0.3.0")],
            cpus: 0,
            busy_ns: 0,
            counter: Some(("network_bytes", 10)),
        }],
    );
    let out = describe(&path, &[]);
    assert!(!out.contains("recorded by an agent before"), "{out}");
    assert!(out.contains("has no Rezolus agent version"), "{out}");
}
