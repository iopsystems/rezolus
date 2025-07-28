use anyhow::{bail, Result};
use std::process::Command;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("fmt") => fmt(),
        Some("docker") => {
            let platform = args.next();
            let tag = args.next();

            // Handle help flag
            if platform.as_deref() == Some("--help") || platform.as_deref() == Some("-h") {
                print_docker_help();
                return Ok(());
            }

            docker(platform, tag)
        }
        _ => {
            eprintln!("Usage: cargo xtask <fmt|docker>");
            eprintln!("  fmt                    - Format Rust and C code");
            eprintln!("  docker [platform] [tag] - Build Docker image");
            eprintln!("                            platform: linux/amd64, linux/arm64, or both (default: both)");
            eprintln!("                            tag: custom tag (default: rezolus:latest)");
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

fn print_docker_help() {
    eprintln!("cargo xtask docker - Build Docker images for Rezolus");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("    cargo xtask docker [PLATFORM] [TAG]");
    eprintln!();
    eprintln!("ARGS:");
    eprintln!(
        "    PLATFORM    Target platform: 'linux/amd64', 'linux/arm64', or 'both' [default: both]"
    );
    eprintln!("    TAG         Docker image tag [default: rezolus:latest]");
    eprintln!();
    eprintln!("EXAMPLES:");
    eprintln!("    cargo xtask docker                        # Build multi-platform image");
    eprintln!("    cargo xtask docker linux/amd64            # Build for AMD64 only");
    eprintln!("    cargo xtask docker both rezolus:v5.2.2    # Multi-platform with custom tag");
}

fn docker(platform: Option<String>, tag: Option<String>) -> Result<()> {
    let tag = tag.unwrap_or_else(|| "rezolus:latest".to_string());

    match platform.as_deref() {
        Some("linux/amd64") => build_docker_single(&tag, "linux/amd64"),
        Some("linux/arm64") => build_docker_single(&tag, "linux/arm64"),
        Some("both") | None => build_docker_multi(&tag),
        Some(p) => {
            eprintln!("Error: Unsupported platform '{}'", p);
            eprintln!();
            print_docker_help();
            bail!("Invalid platform specified");
        }
    }
}

fn build_docker_single(tag: &str, platform: &str) -> Result<()> {
    println!("Building Docker image for platform: {}", platform);

    let mut cmd = Command::new("docker");
    cmd.args(["build", "--platform", platform, "-t", tag, "."]);

    run(&mut cmd)?;
    println!(
        "Successfully built Docker image: {} for platform: {}",
        tag, platform
    );
    Ok(())
}

fn build_docker_multi(tag: &str) -> Result<()> {
    println!("Building multi-platform Docker image...");

    // Check if buildx is available
    let buildx_check = Command::new("docker")
        .args(["buildx", "version"])
        .output()?;

    if !buildx_check.status.success() {
        bail!(
            "Docker buildx is required for multi-platform builds. Please install or enable buildx."
        );
    }

    // Create builder if it doesn't exist
    let builder_name = "rezolus-builder";
    let mut create_builder = Command::new("docker");
    create_builder.args([
        "buildx",
        "create",
        "--name",
        builder_name,
        "--driver",
        "docker-container",
        "--use",
    ]);

    // Ignore error if builder already exists
    let _ = create_builder.status();

    // Use the builder
    let mut use_builder = Command::new("docker");
    use_builder.args(["buildx", "use", builder_name]);
    run(&mut use_builder)?;

    // Build multi-platform image
    let mut cmd = Command::new("docker");
    cmd.args([
        "buildx",
        "build",
        "--platform",
        "linux/amd64,linux/arm64",
        "-t",
        tag,
        "--push", // Push to registry (comment out if building locally)
        ".",
    ]);

    // If not pushing, use --load instead (but load only works for single platform)
    // For local multi-platform builds without push, we need to build separately
    println!("Note: Multi-platform build will push to registry. Use single platform builds for local testing.");

    run(&mut cmd)?;
    println!("Successfully built multi-platform Docker image: {}", tag);
    Ok(())
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status()?;
    if !status.success() {
        bail!("command failed: {:?}", cmd);
    }
    Ok(())
}
