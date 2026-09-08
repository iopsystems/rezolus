//! `include_dir!` embeds `templates/` at compile time, but the macro emits no
//! dependency information, so cargo has no reason to rebuild this crate when a
//! template is added, edited, or removed -- the stale embedded set silently
//! ships, and a newly added service simply does not appear.
//!
//! Declaring the directory here is what makes an edit under `templates/`
//! trigger a rebuild.

fn main() {
    println!("cargo:rerun-if-changed=templates");
}
