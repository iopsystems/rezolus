use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("fmt") => fmt(),
        _ => {
            eprintln!("Usage: cargo xtask [fmt]");
            Ok(())
        }
    }
}

fn fmt() -> Result<()> {
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
        let clang_format = clang_format()?;
        let status = Command::new(&clang_format)
            .arg("-i")
            .args(&files)
            .status()
            .with_context(|| format!("failed to run {}", clang_format.display()))?;
        if !status.success() {
            bail!("clang-format failed");
        }
    }
    Ok(())
}

/// The `clang-format` to run: `$CLANG_FORMAT` if set, else the first one on
/// `PATH`, else Homebrew's LLVM, which does not link its tools onto `PATH`.
fn clang_format() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLANG_FORMAT") {
        return Ok(PathBuf::from(path));
    }
    let on_path = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    let homebrew = [
        PathBuf::from("/opt/homebrew/opt/llvm/bin"),
        PathBuf::from("/usr/local/opt/llvm/bin"),
    ];
    if let Some(found) = on_path
        .into_iter()
        .chain(homebrew)
        .map(|dir| dir.join("clang-format"))
        .find(|path| path.is_file())
    {
        return Ok(found);
    }
    bail!(
        "clang-format not found on PATH or in Homebrew's LLVM; install it \
         (e.g. `brew install llvm` or `apt install clang-format`) or set CLANG_FORMAT"
    )
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        bail!("command failed: {:?}", cmd);
    }
    Ok(())
}
