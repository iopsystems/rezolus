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
    use std::path::{Path, PathBuf};
    // use std::path::PathBuf;

    const SOURCES: &'static [(&str, &str)] = &[
        ("blockio", "biolat"),
        // ("blockio", "cachestat"),
        ("scheduler", "runqlat"),
        ("syscall", "syscall"),
        ("tcp", "receive"),
        ("tcp", "retransmit"),
        ("tcp", "traffic"),
        // ("src/samplers/blockio/blockio.bpf.c", "src/samplers/blockio/blockio.rs"),
        // ("src/samplers/filesystem/fslat.bpf.c", "src/samplers/filesystem/fslat.rs"),
        // ("src/samplers/scheduler/runqlat.bpf.c", "src/samplers/scheduler/runqlat.rs"),
        // ("src/samplers/tcp/rcv_established.bpf.c", "src/samplers/tcp/rcv_established.rs"),
        // ("src/samplers/tcp/retransmit_timer.bpf.c", "src/samplers/tcp/retransmit_timer.rs"),
        // ("src/samplers/tcp/traffic.bpf.c", "src/samplers/tcp/traffic.rs"),
    ];

    pub fn generate() {
        for (sampler, prog) in SOURCES {
            let src = format!("src/samplers/{sampler}/bpf/{prog}/mod.bpf.c");
            let tgt = format!("src/samplers/{sampler}/bpf/{prog}/bpf.rs");
            SkeletonBuilder::new()
                .source(&src)
                .build_and_generate(&tgt)
                .unwrap();
            println!("cargo:rerun-if-changed={src}");
        }

        println!("cargo:rerun-if-changed=src/common/bpf/histogram.h");
        println!("cargo:rerun-if-changed=src/common/bpf/vmlinux.h");
    }
}
