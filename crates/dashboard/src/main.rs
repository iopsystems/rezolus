//! Dump dashboard JSON definitions for inspection/debugging.
//!
//! Usage:
//!   cargo run -p dashboard [OUTPUT_DIR]
//!
//! Without arguments, prints all section JSON to stdout.
//! With a directory argument, writes each section as a pretty-printed JSON file.

use dashboard::Tsdb;
use dashboard::dashboard::generate;
use std::collections::HashMap;

fn main() {
    let output_dir = std::env::args().nth(1);

    let rendered: HashMap<String, String> = generate(&Tsdb::default(), None, &[], None, None);

    match output_dir {
        Some(dir) => write_to_dir(&dir, &rendered),
        None => print_to_stdout(&rendered),
    }
}

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
