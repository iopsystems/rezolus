fn main() {
    #[cfg(all(feature = "bpf", target_os = "linux"))]
    bpf::generate();
}

#[cfg(all(feature = "bpf", target_os = "linux"))]
mod bpf {
    use libbpf_cargo::SkeletonBuilder;

    // `SOURCES` lists all BPF programs and the sampler that contains them.
    // Each entry `(sampler, program)` maps to a unique path in the `samplers`
    // directory.
    const SOURCES: &'static [(&str, &str)] = &[
        ("block_io", "latency"),
        ("scheduler", "runqueue"),
        ("syscall", "latency"),
        ("tcp", "packet_latency"),
        ("tcp", "receive"),
        ("tcp", "retransmit"),
        ("tcp", "traffic"),
    ];

    pub fn generate() {
        let out_dir = std::env::var("OUT_DIR").unwrap();

        for (sampler, prog) in SOURCES {
            let src = format!("src/samplers/{sampler}/{prog}/mod.bpf.c");
            let tgt = format!("{out_dir}/{sampler}_{prog}.bpf.rs");

            #[cfg(target_arch = "x86_64")]
            SkeletonBuilder::new()
                .source(&src)
                .clang_args("-I src/common/bpf/x86_64 -fno-unwind-tables")
                .build_and_generate(&tgt)
                .unwrap();

            #[cfg(target_arch = "aarch64")]
            SkeletonBuilder::new()
                .source(&src)
                .clang_args("-I src/common/bpf/aarch64 -fno-unwind-tables")
                .build_and_generate(&tgt)
                .unwrap();

            #[cfg(all(not(target_arch = "aarch64"), not(target_arch = "x86_64")))]
            panic!("BPF support only available for x86_64 and aarch64 architectures");

            println!("cargo:rerun-if-changed={src}");
        }

        println!("cargo:rerun-if-changed=src/common/bpf/histogram.h");
        println!("cargo:rerun-if-changed=src/common/bpf/vmlinux.h");
    }
}
