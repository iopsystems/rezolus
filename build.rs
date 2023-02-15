#[cfg(not(feature = "bpf"))]
fn main() {}

#[cfg(feature = "bpf")]
fn main() {
    use bpf::*;

    fslat();
    runqlat();
}

#[cfg(feature = "bpf")]
mod bpf {
    use libbpf_cargo::SkeletonBuilder;
    use std::env;
    use std::path::PathBuf;

    pub fn fslat() {
        const SRC: &str = "src/samplers/filesystem/fslat.bpf.c";

        let out = "src/samplers/filesystem/fslat.rs";
        SkeletonBuilder::new()
            .source(SRC)
            .build_and_generate(&out)
            .unwrap();
        println!("cargo:rerun-if-changed={SRC}");
        println!("cargo:rerun-if-changed=src/bpf/bpf.h");
    }

    pub fn runqlat() {
        const SRC: &str = "src/samplers/scheduler/runqlat.bpf.c";

        let out = "src/samplers/scheduler/runqlat.rs";
        SkeletonBuilder::new()
            .source(SRC)
            .build_and_generate(&out)
            .unwrap();
        println!("cargo:rerun-if-changed={SRC}");
        println!("cargo:rerun-if-changed=src/bpf/bpf.h");
    }
}
