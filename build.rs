#[cfg(not(feature = "bpf"))]
fn main() {

}

#[cfg(feature = "bpf")]
fn main() {
    use bpf::*;

    runqlat();
}

#[cfg(feature = "bpf")]
mod bpf {
    use std::env;
    use std::path::PathBuf;
    use libbpf_cargo::SkeletonBuilder;

    pub fn runqlat() {
        const SRC: &str = "src/samplers/scheduler/runqlat.bpf.c";

        let mut out =
            PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR must be set in build script"));
        out.push("runqlat.skel.rs");
        SkeletonBuilder::new()
            .source(SRC)
            .build_and_generate(&out)
            .unwrap();
        println!("cargo:rerun-if-changed={SRC}");
    }
}

