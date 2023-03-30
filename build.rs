fn main() {
    #[cfg(feature = "bpf")]
    bpf::generate();
}

#[cfg(feature = "bpf")]
mod bpf {
    use libbpf_cargo::SkeletonBuilder;

    const SOURCES: &'static [(&str, &str)] = &[
        ("block_io", "latency"),
        ("scheduler", "runqueue"),
        ("syscall", "latency"),
        ("tcp", "receive"),
        ("tcp", "retransmit"),
        ("tcp", "traffic"),
    ];

    pub fn generate() {
        for (sampler, prog) in SOURCES {
            let src = format!("src/samplers/{sampler}/{prog}/mod.bpf.c");
            let tgt = format!("src/samplers/{sampler}/{prog}/bpf.rs");
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
