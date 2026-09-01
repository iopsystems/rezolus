//! Write a small multi-recording `.rez` for tests that cannot build one
//! themselves — the JS-side viewer tests, which drive the built WASM bundle
//! from node and have no way to call into this crate.
//!
//! Usage: `cargo run -p rez --features test-support --example write_rez_fixture -- <path> [n]`
//!
//! `n` (default 2) is how many recordings the archive holds; each gets a
//! distinct `source` label and its own values, so a consumer that picks the
//! wrong one is visible rather than merely "some data came back".

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: write_rez_fixture <path> [recordings]");
    let n: usize = args
        .next()
        .map(|s| s.parse().expect("recording count must be a number"))
        .unwrap_or(2);

    const NAMES: [&str; 4] = ["redis", "valkey", "envoy", "nginx"];
    assert!(n >= 1 && n <= NAMES.len(), "1..={} recordings", NAMES.len());
    let recordings: Vec<(&str, &str)> = NAMES[..n].iter().map(|s| (*s, "web-01")).collect();

    let path = std::path::PathBuf::from(path);
    let _ = std::fs::remove_file(&path);
    rez::rez::recorder_tests_support::multi_recording_v3_rez(&path, &recordings);
    println!("{}", path.display());
}
