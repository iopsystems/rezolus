//! The one query an individual cgroup chart runs for a selection holding
//! groups (`cgroupQueryVariants` in
//! src/viewer/assets/lib/features/cgroup_selection.js), run on a dendro
//! archive. The queries come from tests/fixtures/cgroup_group_query.json,
//! which tests/cgroup_groups.test.mjs checks the JS still generates. Each
//! query must answer with one series per selected name and one per selected
//! group, a group's value computed from its members' sums. The archive also
//! holds the fixture's `unlisted` cgroups, which the selection was not made
//! from, as the other arm of a comparison can hold cgroups the first lacks.

use std::collections::{BTreeMap, HashMap};

use metriken_exposition::Counter;
use metriken_query::QueryResult;

const SECOND: u64 = 1_000_000_000;
const TICKS: u64 = 30;

#[derive(serde::Deserialize)]
struct Fixture {
    names: Vec<String>,
    unlisted: Vec<String>,
    series: BTreeMap<String, Vec<String>>,
    queries: Vec<FixtureQuery>,
}

#[derive(serde::Deserialize)]
struct FixtureQuery {
    template: String,
    query: String,
}

/// Per-second rates of the cgroup at index `i` of the fixture's names:
/// (instructions, cycles, user ns, system ns). Cycles grow faster than
/// instructions with `i`, so a group's ratio of sums differs from the mean
/// of its members' ratios.
fn rates(i: u64) -> (u64, u64, u64, u64) {
    (
        (i + 1) * 1000,
        1000 + i * i * 300,
        (i + 1) * 10,
        (i + 1) * 3,
    )
}

fn counter(metric: &str, name: &str, state: Option<&str>, value: u64) -> Counter {
    let mut meta = HashMap::from([
        ("metric".to_string(), metric.to_string()),
        ("sampler".to_string(), "cgroup".to_string()),
        ("name".to_string(), name.to_string()),
    ]);
    let key = match state {
        Some(state) => {
            meta.insert("state".to_string(), state.to_string());
            format!("{metric}/{name}/{state}")
        }
        None => format!("{metric}/{name}"),
    };
    Counter::new(key, value, meta)
}

#[test]
fn a_cgroup_groups_line_sums_its_members_in_one_query() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/cgroup_group_query.json"
    );
    let fixture: Fixture = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

    let recorded: Vec<&String> = fixture.names.iter().chain(&fixture.unlisted).collect();
    let rows = (0..TICKS)
        .map(|t| {
            let ts = ::rez::fixtures::ANCHOR + t * SECOND;
            let counters = recorded
                .iter()
                .enumerate()
                .flat_map(|(i, name)| {
                    let (ins, cyc, usr, sys) = rates(i as u64);
                    [
                        counter("cgroup_cpu_instructions", name, None, ins * t),
                        counter("cgroup_cpu_cycles", name, None, cyc * t),
                        counter("cgroup_cpu_usage", name, Some("user"), usr * t),
                        counter("cgroup_cpu_usage", name, Some("system"), sys * t),
                    ]
                })
                .collect();
            (::rez::fixtures::snap_counters(ts, counters), ts)
        })
        .collect();
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("cgroups.dendro");
    ::rez::fixtures::write_dendro(
        &archive,
        &[::rez::fixtures::Source::rezolus(rows)],
        None,
        ::rez::fixtures::Finish::Finalized,
    );
    let reader = crate::mcp::open_source(&archive).unwrap();
    let (start, end) = reader.time_range().unwrap();

    let index = |name: &str| recorded.iter().position(|n| *n == name).unwrap() as u64;
    for q in &fixture.queries {
        let ipc = q.template.contains("cgroup_cpu_instructions");
        let expected: BTreeMap<&str, f64> = fixture
            .series
            .iter()
            .map(|(line, members)| {
                let sum = |f: fn((u64, u64, u64, u64)) -> u64| {
                    members.iter().map(|m| f(rates(index(m)))).sum::<u64>() as f64
                };
                let value = if ipc {
                    sum(|r| r.0) / sum(|r| r.1)
                } else {
                    sum(|r| r.2 + r.3)
                };
                (line.as_str(), value)
            })
            .collect();

        let result = reader
            .query_range(&q.query, start + 5.0, end, 1.0)
            .unwrap_or_else(|e| panic!("{}: {e}", q.template));
        let QueryResult::Matrix { result, .. } = result else {
            panic!("{}: not a matrix", q.template);
        };
        let got: BTreeMap<&str, f64> = result
            .iter()
            .map(|s| {
                assert_eq!(s.metric.len(), 1, "{}: {:?}", q.template, s.metric);
                (s.metric["name"].as_str(), s.values.last().unwrap().1)
            })
            .collect();
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            expected.keys().collect::<Vec<_>>(),
            "{}",
            q.template
        );
        for (line, want) in &expected {
            let have = got[line];
            assert!(
                (have - want).abs() <= want.abs() * 1e-9,
                "{}: {line} is {have}, want {want}",
                q.template
            );
        }
    }
}
