fn main() {
    #[cfg(feature = "bpf")]
    bpf::generate();

    #[cfg(target_os = "linux")]
    perf::generate();
}

#[cfg(feature = "bpf")]
mod bpf {
    use libbpf_cargo::SkeletonBuilder;
    // use std::env;
    // use std::path::{Path, PathBuf};
    // use std::path::PathBuf;

    const SOURCES: &'static [(&str, &str)] = &[
        ("block_io", "latency"),
        // ("blockio", "cachestat"),
        ("scheduler", "runqueue"),
        ("syscall", "latency"),
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

#[cfg(target_os = "linux")]
mod perf {
    pub fn generate() {
        println!("cargo:rerun-if-changed=src/common/perf/wrapper.h");

        let bindings = bindgen::Builder::default()
            .header("src/common/perf/wrapper.h")
            .parse_callbacks(Box::new(bindgen::CargoCallbacks))
            .generate()
            .expect("Unable to generate bindings");

        // Write the bindings to the $OUT_DIR/bindings.rs file.
        let out_path = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        bindings
            .write_to_file(out_path.join("perf_event_sys.rs"))
            .expect("Couldn't write bindings!");
    }
    
    
}
