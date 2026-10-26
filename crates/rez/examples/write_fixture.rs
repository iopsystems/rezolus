//! Write a small multi-recording dendro archive for tests that cannot build
//! one themselves — the JS-side viewer tests, which drive the built WASM
//! bundle from node and have no way to call into this crate.
//!
//! Usage: `cargo run -p rez --features test-support --example write_fixture -- <path> [n] [arms]`
//!
//! `n` (default 2) is how many recordings the archive holds; each gets a
//! distinct `source` label (in its labels and its metadata) and its own
//! values, so a consumer that picks the wrong one is visible rather than
//! merely "some data came back". Each holds three rows of a `cpu_usage`
//! counter `cpu_cycles`, `(i + 1) * 1000 + t` for recording `i` at tick `t`.
//! `arms`, a comma-separated list such as `baseline,experiment`, labels
//! recording `i` with `arm=` its `i`th entry; an empty entry leaves it
//! without one.

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: write_fixture <path> [recordings] [arms]");
    let n: usize = args
        .next()
        .map(|s| s.parse().expect("recording count must be a number"))
        .unwrap_or(2);
    let arms: Vec<String> = args
        .next()
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default();

    const NAMES: [&str; 4] = ["redis", "valkey", "envoy", "nginx"];
    assert!(n >= 1 && n <= NAMES.len(), "1..={} recordings", NAMES.len());
    let recordings: Vec<(&str, &str)> = NAMES[..n].iter().map(|s| (*s, "web-01")).collect();

    let path = std::path::PathBuf::from(path);
    let _ = std::fs::remove_file(&path);
    let mut sources = rez::fixtures::multi_recording_sources(&recordings);
    for (source, arm) in sources.iter_mut().zip(&arms) {
        if !arm.is_empty() {
            source.labels.insert("arm".to_string(), arm.clone());
        }
    }
    rez::fixtures::write_dendro(&path, &sources, None, rez::fixtures::Finish::Finalized);
    println!("{}", path.display());
}
