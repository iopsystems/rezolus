//! Dump dashboard JSON definitions for inspection/debugging.
//!
//! Usage:
//!   cargo run -p dashboard [OUTPUT_DIR]
//!
//! Without arguments, prints all section JSON to stdout.
//! With a directory argument, writes each section as a pretty-printed JSON file.

// The dump binary still drives generators with a `Tsdb::default()`
// stand-in — the `DashboardData` trait is what `generate_section`
// actually consumes, but no SQL-side equivalent exists for the
// empty-context case yet. Gate behind `live-mode` so the SQL-only
// configuration of the workspace doesn't try to compile this binary.
#[cfg(not(feature = "live-mode"))]
fn main() {
    eprintln!("`cargo run -p dashboard` requires the live-mode feature (Tsdb-backed dump).");
    std::process::exit(1);
}

#[cfg(feature = "live-mode")]
use dashboard::Tsdb;
#[cfg(feature = "live-mode")]
use dashboard::dashboard::{build_dashboard_context, generate_section};
#[cfg(feature = "live-mode")]
use std::collections::HashMap;

#[cfg(feature = "live-mode")]
fn main() {
    let output_dir = std::env::args().nth(1);

    // Render every section in the navigation list. The lazy API replaces
    // the old eager `generate` shim — same coverage, just hand-walked.
    let data = Tsdb::default();
    let ctx = build_dashboard_context(None, &[], None);
    let mut rendered: HashMap<String, String> = HashMap::new();
    for section in &ctx.sections {
        if let Some(view) = generate_section(&data, &section.route, &ctx) {
            let key = format!("{}.json", &section.route[1..]);
            rendered.insert(key, serde_json::to_string(&view).unwrap());
        }
    }

    match output_dir {
        Some(dir) => write_to_dir(&dir, &rendered),
        None => print_to_stdout(&rendered),
    }
}

#[cfg(feature = "live-mode")]
fn print_to_stdout(rendered: &HashMap<String, String>) {
    let mut keys: Vec<&String> = rendered.keys().collect();
    keys.sort();

    for key in keys {
        let json = &rendered[key];
        // Pretty-print if valid JSON, otherwise dump raw
        match serde_json::from_str::<serde_json::Value>(json) {
            Ok(val) => {
                println!("--- {} ---", key);
                println!("{}", serde_json::to_string_pretty(&val).unwrap());
            }
            Err(_) => {
                println!("--- {} ---", key);
                println!("{}", json);
            }
        }
    }
}

#[cfg(feature = "live-mode")]
fn write_to_dir(dir: &str, rendered: &HashMap<String, String>) {
    let path = std::path::Path::new(dir);
    std::fs::create_dir_all(path).expect("failed to create output directory");

    // Extract sections list from the first entry and write it separately
    let mut sections_written = false;
    let mut keys: Vec<&String> = rendered.keys().collect();
    keys.sort();

    for key in &keys {
        let json = &rendered[*key];
        let mut value: serde_json::Value =
            serde_json::from_str(json).expect("invalid JSON in rendered output");

        if !sections_written && let Some(sections) = value.get("sections") {
            let sections_path = path.join("sections.json");
            let pretty = serde_json::to_string_pretty(sections).unwrap();
            std::fs::write(&sections_path, &pretty).unwrap();
            eprintln!("wrote {}", sections_path.display());
            sections_written = true;
        }

        // Remove sections from per-dashboard files to reduce duplication
        if let Some(obj) = value.as_object_mut() {
            obj.remove("sections");
        }

        let file_path = path.join(key);
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        std::fs::write(&file_path, &pretty).unwrap();
        eprintln!("wrote {}", file_path.display());
    }
}
