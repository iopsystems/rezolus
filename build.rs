#[cfg(not(feature = "bpf"))]
fn main() {}

#[cfg(feature = "bpf")]
fn main() {
    use bpf::*;

    generate();
}

#[cfg(feature = "bpf")]
mod bpf {
    use libbpf_cargo::SkeletonBuilder;
    use std::env;
    use std::path::PathBuf;

    const SOURCES: &'static [(&str, &str)] = &[
        (
            "src/samplers/tcp/bpf/traffic/traffic.bpf.c",
            "src/samplers/tcp/bpf/traffic/bpf.rs",
        ),
        // ("src/samplers/blockio/blockio.bpf.c", "src/samplers/blockio/blockio.rs"),
        // ("src/samplers/filesystem/fslat.bpf.c", "src/samplers/filesystem/fslat.rs"),
        // ("src/samplers/scheduler/runqlat.bpf.c", "src/samplers/scheduler/runqlat.rs"),
        // ("src/samplers/tcp/rcv_established.bpf.c", "src/samplers/tcp/rcv_established.rs"),
        // ("src/samplers/tcp/retransmit_timer.bpf.c", "src/samplers/tcp/retransmit_timer.rs"),
        // ("src/samplers/tcp/traffic.bpf.c", "src/samplers/tcp/traffic.rs"),
    ];

    pub fn generate() {
        for (source, target) in SOURCES {
            SkeletonBuilder::new()
                .source(source)
                .build_and_generate(target)
                .unwrap();
            println!("cargo:rerun-if-changed={source}");
        }

        println!("cargo:rerun-if-changed=src/common/bpf/histogram.h");
        println!("cargo:rerun-if-changed=src/common/bpf/vmlinux.h");
    }
}
