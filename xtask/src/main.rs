use std::process::Command;
use anyhow::{bail, Result};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("fmt") => fmt(),
        _ => {
            eprintln!("Usage: cargo xtask fmt");
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

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        bail!("command failed: {:?}", cmd);
    }
    Ok(())
}
