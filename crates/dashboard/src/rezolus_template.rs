//! The section template for a Rezolus agent's metrics.
//!
//! Each section is a file under `sections/rezolus/`, and [`json`] assembles
//! them, in navigation order, into one schema 1 document. A file holding
//! `null` is a section not yet converted from its Rust generator, and is
//! left out.

/// Each section's file, in navigation order. The query explorer is the
/// library section, placed after Exceptions as the generators place it.
const PARTS: &[(&str, &str)] = &[
    (
        "overview",
        include_str!("../sections/rezolus/overview.json"),
    ),
    (
        "exceptions",
        include_str!("../sections/rezolus/exceptions.json"),
    ),
    ("query", r#"{"builtin": "query"}"#),
    ("cpu", include_str!("../sections/rezolus/cpu.json")),
    ("gpu", include_str!("../sections/rezolus/gpu.json")),
    (
        "hw_sensors",
        include_str!("../sections/rezolus/hw_sensors.json"),
    ),
    ("memory", include_str!("../sections/rezolus/memory.json")),
    ("network", include_str!("../sections/rezolus/network.json")),
    (
        "scheduler",
        include_str!("../sections/rezolus/scheduler.json"),
    ),
    ("syscall", include_str!("../sections/rezolus/syscall.json")),
    ("softirq", include_str!("../sections/rezolus/softirq.json")),
    ("blockio", include_str!("../sections/rezolus/blockio.json")),
    (
        "filesystem",
        include_str!("../sections/rezolus/filesystem.json"),
    ),
    ("ext4", include_str!("../sections/rezolus/ext4.json")),
    ("xfs", include_str!("../sections/rezolus/xfs.json")),
    ("cgroups", include_str!("../sections/rezolus/cgroups.json")),
    ("rezolus", include_str!("../sections/rezolus/rezolus.json")),
];

/// The template document, as the agent sends it.
pub fn json() -> String {
    let sections: Vec<serde_json::Value> = PARTS
        .iter()
        .map(|(name, part)| {
            serde_json::from_str(part)
                .unwrap_or_else(|e| panic!("sections/rezolus/{name}.json: {e}"))
        })
        .filter(|v: &serde_json::Value| !v.is_null())
        .collect();
    serde_json::json!({
        "schema": 1,
        "revision": "rezolus agent",
        "sections": sections,
    })
    .to_string()
}
