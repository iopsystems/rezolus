use std::process::Command;
use anyhow::{bail, Result};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("fmt") => fmt(),
        Some("generate-dashboards") => generate_dashboards(args),
        _ => {
            eprintln!("Usage: cargo xtask [fmt | generate-dashboards [--check]]");
            Ok(())
        }
    }
}

fn fmt() -> Result<()> {
    // rustfmt
    run(Command::new("cargo").arg("fmt").arg("--all"))?;

    // clang-format on tracked C/H files excluding vmlinux.h
    let output = Command::new("git")
        .args(["ls-files", "*.c", "*.h"])
        .output()?;
    if !output.status.success() {
        bail!("git ls-files failed");
    }
    let files: Vec<String> = String::from_utf8(output.stdout)?
        .lines()
        .filter(|f| !f.ends_with("vmlinux.h"))
        .map(|s| s.to_string())
        .collect();
    if !files.is_empty() {
        let status = Command::new("clang-format")
            .arg("-i")
            .args(&files)
            .status()?;
        if !status.success() {
            bail!("clang-format failed");
        }
    }
    Ok(())
}

fn generate_dashboards(args: impl Iterator<Item = String>) -> Result<()> {
    let check = args.into_iter().any(|a| a == "--check");

    let output_dir = if check {
        std::env::temp_dir().join("rezolus-dashboards-check")
    } else {
        std::path::PathBuf::from("site/viewer/dashboards")
    };

    run(Command::new("cargo")
        .args(["run", "--features", "xtask-commands", "--", "dump-dashboards"])
        .arg(&output_dir))?;

    if check {
        let committed = std::path::Path::new("site/viewer/dashboards");
        let status = Command::new("diff")
            .args(["-r", "-q"])
            .arg(committed)
            .arg(&output_dir)
            .status()?;
        let _ = std::fs::remove_dir_all(&output_dir);
        if !status.success() {
            bail!(
                "Dashboard JSON is out of date. Run `cargo xtask generate-dashboards` to update."
            );
        }
        eprintln!("Dashboards are up to date.");
    }

    Ok(())
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        bail!("command failed: {:?}", cmd);
    }
    Ok(())
}
