mod builder;
mod counters;
pub mod drivers;
pub mod filesystems;
mod histogram;
mod sync_primitive;

pub use builder::Builder as BpfBuilder;
pub use builder::{BpfProgStats, PerfEvent};

use std::path::Path;
use std::sync::OnceLock;

use crate::agent::samplers::Sampler;
use crate::agent::timing::AcquisitionGroup;
use crate::*;

/// Returns true if the running kernel exposes its own BTF
/// (`/sys/kernel/btf/vmlinux`). Programs that need in-kernel BTF — `tp_btf`,
/// `fentry`/`fexit`, and the `bpf_get_current_task_btf` helper — can only be
/// attached when this is true. Checked once and cached.
pub fn kernel_has_btf() -> bool {
    static HAS_BTF: OnceLock<bool> = OnceLock::new();
    *HAS_BTF.get_or_init(|| Path::new("/sys/kernel/btf/vmlinux").exists())
}

/// Returns true if the running kernel's BTF describes every one of `names` as a
/// function — i.e. an `fentry`/`fexit` program may name it as an attach target.
///
/// [`kernel_has_btf`] answers a different, weaker question: "is there a vmlinux
/// BTF at all". A trampoline program additionally needs ITS OWN target in that
/// BTF, and libbpf resolves `attach_btf_id` at LOAD time — so a missing target
/// is a load failure, which [`BpfBuilder`] treats as fatal for the whole
/// skeleton (`set_failed`, and `rezolus status` then exits non-zero). A kprobe's
/// equivalent miss is an ENOENT at ATTACH, which is tolerated and reported as
/// unsupported.
///
/// That difference matters for any hook sitting behind a kernel config option:
/// `cpu_bandwidth`'s three targets are all inside `CONFIG_CFS_BANDWIDTH`, so
/// choosing its trampoline twins on `kernel_has_btf()` alone would turn "this
/// kernel does not implement CFS bandwidth control" into a failed sampler.
/// Selecting on this function instead falls back to the kprobe twins, which miss
/// gracefully.
///
/// Module BTF counts too: libbpf resolves an `fentry`/`fexit` target against
/// every loaded module's BTF (kernels 5.11+), so a function that lives in
/// `ext4.ko` is attachable when `/sys/kernel/btf/ext4` describes it.
///
/// Not cached: callers ask once, at sampler init, and each call parses the
/// kernel's BTF. Returns false when there is no kernel BTF to consult.
pub fn kernel_btf_has_funcs(names: &[&str]) -> bool {
    if !kernel_has_btf() {
        return false;
    }

    let Some(vmlinux) = RawBtf::parse(Path::new("/sys/kernel/btf/vmlinux"), None) else {
        return false;
    };

    // Declared after `vmlinux` so it drops first: a split BTF refers to its
    // base for the whole of its life.
    let modules: Vec<RawBtf> = module_btf_paths(Path::new("/sys/kernel/btf"))
        .iter()
        .filter_map(|path| RawBtf::parse(path, Some(&vmlinux)))
        .collect();

    // Every name is checked, not just up to the first miss: when a sampler is
    // about to fall back, the useful debug output names all of what is absent.
    let mut all = true;

    for name in names {
        if !vmlinux.has_func(name) && !modules.iter().any(|m| m.has_func(name)) {
            debug!("kernel BTF (vmlinux and modules) has no function `{name}`");
            all = false;
        }
    }

    all
}

/// The number of parameters kernel function `name` takes, from its BTF
/// prototype in vmlinux or a module, or `None` when no BTF describes it.
///
/// The `fentry`/`fexit` counterpart of [`kernel_btf_tracepoint_arg_count`]: a
/// function's signature can change between kernel versions (`ext4_rename2`
/// gained a namespace argument in 5.12), a trampoline program reads its
/// arguments and, for `fexit`, the return value by position, and a program
/// written for one arity reads the wrong slot on the other without an error.
pub fn kernel_btf_func_arg_count(name: &str) -> Option<u32> {
    if !kernel_has_btf() {
        return None;
    }

    let vmlinux = RawBtf::parse(Path::new("/sys/kernel/btf/vmlinux"), None)?;

    if let Some(n) = vmlinux.func_arg_count(name) {
        return Some(n);
    }

    module_btf_paths(Path::new("/sys/kernel/btf"))
        .iter()
        .filter_map(|path| RawBtf::parse(path, Some(&vmlinux)))
        .find_map(|btf| btf.func_arg_count(name))
}

/// Returns true if the running kernel's BTF — vmlinux **or any loaded
/// module's** — describes every one of `names` as a tracepoint, i.e. carries
/// its `btf_trace_<name>` typedef, so a `tp_btf` program may name it as an
/// attach target.
///
/// [`kernel_btf_has_funcs`] answers the same question for `fentry`/`fexit`
/// targets. Module BTF matters for both: on a kernel with `CONFIG_EXT4_FS=m`
/// (stock Debian amd64, for one), `btf_trace_jbd2_run_stats` is in
/// `/sys/kernel/btf/jbd2`, not in vmlinux, and libbpf does resolve `tp_btf`
/// targets against module BTF (kernels 5.11+). Selecting the `tp_btf`
/// twin on [`kernel_has_btf`] alone would make that kernel a load failure,
/// fatal for the whole skeleton (see [`kernel_btf_has_funcs`] for why load-time
/// misses matter more than attach-time ones). Selecting on this function falls
/// back to the `raw_tp` twin, whose attach failure on a kernel without the
/// tracepoint at all is an ENOENT the builder tolerates.
///
/// Not cached: callers ask once per hook at sampler init. Returns false when
/// there is no kernel BTF to consult.
pub fn kernel_btf_has_tracepoints(names: &[&str]) -> bool {
    if !kernel_has_btf() {
        return false;
    }

    let Some(vmlinux) = RawBtf::parse(Path::new("/sys/kernel/btf/vmlinux"), None) else {
        return false;
    };

    // Declared after `vmlinux` so it drops first: a split BTF refers to its
    // base for the whole of its life.
    let modules: Vec<RawBtf> = module_btf_paths(Path::new("/sys/kernel/btf"))
        .iter()
        .filter_map(|path| RawBtf::parse(path, Some(&vmlinux)))
        .collect();

    let mut all = true;

    for name in names {
        let typedef = format!("btf_trace_{name}");

        if !vmlinux.has_typedef(&typedef) && !modules.iter().any(|m| m.has_typedef(&typedef)) {
            debug!("kernel BTF (vmlinux and modules) has no tracepoint `{name}`");
            all = false;
        }
    }

    all
}

/// The module BTF files under `dir` (`/sys/kernel/btf/<module>`, one per
/// loaded module with BTF), `vmlinux` excluded. A directory that does not
/// exist yields nothing rather than an error: no module BTF is the normal
/// state on a kernel without `CONFIG_DEBUG_INFO_BTF_MODULES`.
fn module_btf_paths(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .filter(|entry| entry.file_name() != "vmlinux")
        .map(|entry| entry.path())
        .collect()
}

/// An owned libbpf BTF object. Exists because `libbpf_rs::btf::Btf::from_path`
/// parses standalone BTF only: a module's BTF is *split* BTF whose type ids
/// continue vmlinux's, and libbpf refuses it without the base
/// (`btf__parse_split`), which libbpf-rs 0.26 does not expose.
struct RawBtf(std::ptr::NonNull<libbpf_sys::btf>);

impl RawBtf {
    /// Parse the BTF at `path`; `base` is the vmlinux BTF for a module's split
    /// BTF, `None` for vmlinux itself. `None` on any parse failure — libbpf 1.x
    /// returns a null pointer and sets errno.
    fn parse(path: &Path, base: Option<&RawBtf>) -> Option<Self> {
        use std::os::unix::ffi::OsStrExt;

        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;

        // SAFETY: `cpath` is a valid NUL-terminated string for the call, and
        // `base`, when given, is a live BTF object this function does not free.
        let ptr = unsafe {
            match base {
                Some(base) => libbpf_sys::btf__parse_split(cpath.as_ptr(), base.0.as_ptr()),
                None => libbpf_sys::btf__parse(cpath.as_ptr(), std::ptr::null_mut()),
            }
        };

        std::ptr::NonNull::new(ptr).map(Self)
    }

    /// Whether this BTF declares a typedef of that name. For a module's
    /// split BTF the search covers its base too, so a hit may come from
    /// vmlinux; callers check vmlinux first, so the answer is the same.
    fn has_typedef(&self, name: &str) -> bool {
        self.find(name, libbpf_sys::BTF_KIND_TYPEDEF) >= 0
    }

    /// Whether this BTF declares a function of that name (base included, as
    /// for [`has_typedef`](Self::has_typedef)).
    fn has_func(&self, name: &str) -> bool {
        self.find(name, libbpf_sys::BTF_KIND_FUNC) >= 0
    }

    /// The id of the type named `name` of `kind` in this BTF, or negative.
    /// `btf__find_by_name_kind` walks from id 1, so on a split BTF it walks
    /// the base's types as well; a miss therefore costs a walk of vmlinux per
    /// module, paid once at sampler init.
    fn find(&self, name: &str, kind: u32) -> i32 {
        let Ok(cname) = std::ffi::CString::new(name) else {
            return -1;
        };

        // SAFETY: `self.0` is a live BTF object and `cname` a valid C string.
        unsafe { libbpf_sys::btf__find_by_name_kind(self.0.as_ptr(), cname.as_ptr(), kind) }
    }

    /// The number of parameters function `name` takes, from its BTF: a
    /// `FUNC` refers to a `FUNC_PROTO` whose vlen is the parameter count.
    /// `None` if this BTF has no such function.
    fn func_arg_count(&self, name: &str) -> Option<u32> {
        let id = self.find(name, libbpf_sys::BTF_KIND_FUNC);
        if id < 0 {
            return None;
        }

        // SAFETY: `self.0` is a live BTF object; every id passed to
        // `btf__type_by_id` came from that object, and the returned pointer is
        // read before the object is freed.
        unsafe {
            let func = libbpf_sys::btf__type_by_id(self.0.as_ptr(), id as u32);
            if func.is_null() || btf_kind(&*func) != libbpf_sys::BTF_KIND_FUNC {
                return None;
            }
            let proto =
                libbpf_sys::btf__type_by_id(self.0.as_ptr(), (*func).__bindgen_anon_1.type_);
            if proto.is_null() || btf_kind(&*proto) != libbpf_sys::BTF_KIND_FUNC_PROTO {
                return None;
            }
            Some((*proto).info & 0xffff)
        }
    }

    /// The number of arguments the tracepoint `name` passes to a `tp_btf` or
    /// `raw_tp` program, from its `btf_trace_<name>` typedef: the typedef
    /// names a pointer to a function prototype whose first parameter is the
    /// tracepoint's `void *` context and whose remaining parameters are the
    /// `TP_PROTO` arguments, in order. `None` if this BTF has no such
    /// typedef or it is not shaped that way.
    fn tracepoint_arg_count(&self, name: &str) -> Option<u32> {
        let Ok(cname) = std::ffi::CString::new(format!("btf_trace_{name}")) else {
            return None;
        };

        // SAFETY: `self.0` is a live BTF object; every id passed to
        // `btf__type_by_id` came from that object, and the returned pointer is
        // read before the object is freed.
        unsafe {
            let id = libbpf_sys::btf__find_by_name_kind(
                self.0.as_ptr(),
                cname.as_ptr(),
                libbpf_sys::BTF_KIND_TYPEDEF,
            );
            if id < 0 {
                return None;
            }

            // typedef -> ptr -> func_proto; `type` on the first two is the
            // referenced type id, and vlen on the last is the parameter count.
            let mut ty = libbpf_sys::btf__type_by_id(self.0.as_ptr(), id as u32);
            for expected in [libbpf_sys::BTF_KIND_TYPEDEF, libbpf_sys::BTF_KIND_PTR] {
                if ty.is_null() || btf_kind(&*ty) != expected {
                    return None;
                }
                ty = libbpf_sys::btf__type_by_id(self.0.as_ptr(), (*ty).__bindgen_anon_1.type_);
            }
            if ty.is_null() || btf_kind(&*ty) != libbpf_sys::BTF_KIND_FUNC_PROTO {
                return None;
            }

            let params = (*ty).info & 0xffff;
            params.checked_sub(1)
        }
    }
}

/// The kind bits of a BTF type's `info` word (bits 24..29), as
/// `BTF_INFO_KIND` computes them.
fn btf_kind(ty: &libbpf_sys::btf_type) -> u32 {
    (ty.info >> 24) & 0x1f
}

/// The number of arguments tracepoint `name` passes to a `tp_btf`/`raw_tp`
/// program on the running kernel, from vmlinux or module BTF, or `None` when
/// no BTF describes it.
///
/// A tracepoint's `TP_PROTO` can change between kernel versions, and a
/// `tp_btf`/`raw_tp` program reads its arguments by position, so a program
/// written for one arity reads the wrong argument on a kernel with the other
/// without any error. A sampler that hooks such a tracepoint keeps one program
/// per known arity and selects on this at init; an unknown arity disables the
/// hook rather than guessing. `kernel_btf_has_tracepoints` is the existence
/// check this refines.
pub fn kernel_btf_tracepoint_arg_count(name: &str) -> Option<u32> {
    if !kernel_has_btf() {
        return None;
    }

    let vmlinux = RawBtf::parse(Path::new("/sys/kernel/btf/vmlinux"), None)?;

    if let Some(n) = vmlinux.tracepoint_arg_count(name) {
        return Some(n);
    }

    module_btf_paths(Path::new("/sys/kernel/btf"))
        .iter()
        .filter_map(|path| RawBtf::parse(path, Some(&vmlinux)))
        .find_map(|btf| btf.tracepoint_arg_count(name))
}

impl Drop for RawBtf {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `btf__parse`/`btf__parse_split` and is
        // freed exactly once, here.
        unsafe { libbpf_sys::btf__free(self.0.as_ptr()) }
    }
}

#[cfg(test)]
mod btf_tests {
    use super::{module_btf_paths, RawBtf};
    use std::path::Path;

    /// A kernel without module BTF has no `/sys/kernel/btf/<module>` files;
    /// the scan must then contribute nothing, not fail.
    #[test]
    fn a_missing_module_btf_dir_yields_no_paths() {
        assert!(module_btf_paths(Path::new("/nonexistent/rezolus/btf")).is_empty());
    }

    /// A file that is not BTF parses to nothing rather than aborting.
    #[test]
    fn a_non_btf_file_does_not_parse() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(RawBtf::parse(&manifest, None).is_none());
    }

    /// On a kernel with its own BTF, vmlinux declares the scheduler's
    /// tracepoints, and a module's split BTF parses against it. Skipped
    /// where there is no kernel BTF.
    #[test]
    fn vmlinux_btf_has_sched_switch_and_modules_parse_against_it() {
        let vmlinux_path = Path::new("/sys/kernel/btf/vmlinux");
        if !vmlinux_path.exists() {
            return;
        }
        let vmlinux = RawBtf::parse(vmlinux_path, None).expect("vmlinux BTF parses");
        assert!(vmlinux.has_typedef("btf_trace_sched_switch"));
        assert!(!vmlinux.has_typedef("btf_trace_no_such_tracepoint"));
        // sched_switch has passed 3 (through 5.13) or 4 arguments (5.14+,
        // `prev_state` added); any other count means the walk is wrong.
        let n = vmlinux
            .tracepoint_arg_count("sched_switch")
            .expect("sched_switch is a tracepoint");
        assert!((3..=4).contains(&n), "sched_switch has {n} arguments");
        assert_eq!(vmlinux.tracepoint_arg_count("no_such_tracepoint"), None);
        // vfs_fsync_range(struct file *, loff_t, loff_t, int) on every kernel
        // since 2.6; the count is the FUNC_PROTO's vlen, not vlen - 1 as for a
        // tracepoint's typedef (no leading context pointer).
        assert!(vmlinux.has_func("vfs_fsync_range"));
        assert_eq!(vmlinux.func_arg_count("vfs_fsync_range"), Some(4));
        assert_eq!(vmlinux.func_arg_count("no_such_function"), None);
        for path in module_btf_paths(Path::new("/sys/kernel/btf")) {
            assert!(
                RawBtf::parse(&path, Some(&vmlinux)).is_some(),
                "{} did not parse as split BTF",
                path.display()
            );
        }
    }
}

/// Whether the running kernel lets tracing programs (`tp_btf`, `fentry`,
/// `fexit`) call `bpf_task_storage_get`. The helper and its map type arrived
/// in 5.11 for LSM programs and were opened to tracing programs in 5.12, so a
/// sampler that keeps per-thread state in task local storage cannot load on
/// 5.8–5.11 and asks this at init to report *unsupported* rather than fail.
///
/// This answers only that question. libbpf reports 0 when the verifier names
/// the helper as unknown for the program type, and 1 for every other outcome
/// of loading its two-instruction probe, a permission failure or a kernel
/// without `bpf()` included: on such a host this returns `true` and the
/// sampler goes on to fail at load with the real error, which is what it did
/// before the probe existed.
pub fn kernel_tracing_has_task_storage() -> bool {
    let ret = probe_task_storage_helper();
    if ret != 1 {
        debug!("kernel BPF helper probe for task storage from tracing programs returned {ret}");
    }
    ret == 1
}

/// libbpf's raw answer: 1 (supported, or the load failed for a reason other
/// than the helper), 0 (the verifier does not know the helper for this
/// program type), or a negative errno when libbpf refuses the probe itself.
///
/// Probed as a `kprobe` program, not a `tracing` one: libbpf cannot load a
/// standalone `BPF_PROG_TYPE_TRACING` probe and returns `-EOPNOTSUPP` for that
/// type without asking the kernel (`libbpf_probes.c`), which would report
/// every kernel as unsupported. The kernel answers helper availability for
/// `kprobe`, `tracepoint`, `raw_tracepoint` and `tracing` programs from the
/// same table (`bpf_tracing_func_proto`), and the commit that opened task
/// storage to tracing programs added it there, so the `kprobe` probe is the
/// same question.
fn probe_task_storage_helper() -> i32 {
    // SAFETY: FFI call with plain enum arguments and a null options pointer,
    // which libbpf documents as "default options".
    unsafe {
        libbpf_sys::libbpf_probe_bpf_helper(
            libbpf_sys::BPF_PROG_TYPE_KPROBE,
            libbpf_sys::BPF_FUNC_task_storage_get,
            std::ptr::null(),
        )
    }
}

#[cfg(test)]
mod task_storage_probe_tests {
    use super::probe_task_storage_helper;

    /// The probe must reach the kernel. `-EOPNOTSUPP` (-95) is libbpf refusing
    /// the program type before any syscall, which is what happened with
    /// `BPF_PROG_TYPE_TRACING` and made every kernel look unsupported. For the
    /// `kprobe` type libbpf returns only 0 or 1, unprivileged runs included
    /// (a load refused with EPERM writes no verifier log and counts as 1), so
    /// the assertion holds wherever the tests run and fails only on the
    /// regression it guards.
    #[test]
    fn the_task_storage_probe_is_answered_by_the_kernel_not_refused_by_libbpf() {
        let ret = probe_task_storage_helper();
        assert_ne!(
            ret,
            -libc::EOPNOTSUPP,
            "libbpf refused to probe this program type"
        );
    }
}

/// The length of one jiffy in nanoseconds, or `None` if the kernel would not
/// say. `CLOCK_MONOTONIC_COARSE` is the tick-granular clock and its resolution
/// is `TICK_NSEC`: 4,000,000 ns on a `CONFIG_HZ=250` kernel, 1,000,000 on
/// `HZ=1000`. Measured, not configured, so it needs no `/proc/config.gz`; and
/// not `sysconf(_SC_CLK_TCK)`, which is `USER_HZ`, a constant 100.
///
/// For BPF programs whose tracepoint arguments are in jiffies (jbd2's commit
/// phases, the writeback throttle's `pause`): hand this to the program through
/// a one-entry `BPF_F_MMAPABLE` map and multiply there.
pub fn jiffy_ns() -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };

    // SAFETY: `ts` is a valid, writable timespec for the duration of the call.
    let rc = unsafe { libc::clock_getres(libc::CLOCK_MONOTONIC_COARSE, &mut ts) };

    if rc != 0 || ts.tv_sec < 0 || ts.tv_nsec < 0 {
        return None;
    }

    let ns = (ts.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec as u64);

    (ns > 0).then_some(ns)
}

#[cfg(test)]
mod jiffy_tests {
    /// The tick is a real kernel constant: between 1 ms (HZ=1000) and 10 ms
    /// (HZ=100) on every Linux the agent supports. A value outside that range
    /// means the clock being asked is not the tick-granular one.
    #[test]
    fn jiffy_is_between_one_and_ten_milliseconds() {
        let tick = super::jiffy_ns().expect("clock_getres(CLOCK_MONOTONIC_COARSE)");
        assert!(
            (1_000_000..=10_000_000).contains(&tick),
            "tick {tick} ns is not a jiffy"
        );
    }
}

pub trait OpenSkelExt {
    /// When called, the SkelBuilder should log instruction counts for each of
    /// the programs within the skeleton. Log level should be debug.
    fn log_prog_instructions(&self);
}

pub trait SkelExt {
    fn map(&self, name: &str) -> &libbpf_rs::Map<'_>;
}

pub trait CgroupInfo {
    fn id(&self) -> i32;
    fn level(&self) -> i32;
    fn name(&self) -> &[u8];
    fn pname(&self) -> &[u8];
    fn gpname(&self) -> &[u8];
}

#[macro_export]
macro_rules! impl_cgroup_info {
    ($type:ty) => {
        impl $crate::agent::bpf::CgroupInfo for $type {
            fn id(&self) -> i32 {
                self.id
            }

            fn level(&self) -> i32 {
                self.level
            }

            fn name(&self) -> &[u8] {
                &self.name
            }

            fn pname(&self) -> &[u8] {
                &self.pname
            }

            fn gpname(&self) -> &[u8] {
                &self.gpname
            }
        }
    };
}

const CACHELINE_SIZE: usize = 64;
const PAGE_SIZE: usize = 4096;

const COUNTER_SIZE: usize = std::mem::size_of::<u64>();
const COUNTERS_PER_CACHELINE: usize = CACHELINE_SIZE / COUNTER_SIZE;

fn whole_cachelines<T>(count: usize) -> usize {
    (count * std::mem::size_of::<T>()).div_ceil(CACHELINE_SIZE)
}

fn whole_pages<T>(count: usize) -> usize {
    (count * std::mem::size_of::<T>()).div_ceil(PAGE_SIZE)
}

use counters::{Counters, CpuCounters, FilesystemCounters, PackedCounters};
use histogram::{Histogram, HistogramBatch};
pub use sync_primitive::SyncPrimitive;

/// The CPU ids this host actually has, for declaring a per-CPU group's
/// membership.
///
/// **Not `0..possible_cpus()`.** That is a dense prefix over
/// `/sys/devices/system/cpu/possible`, which the kernel populates with ids
/// that *could* be hot-added rather than ids that exist — so on a VM
/// advertising hotplug capacity (`possible: 0-255`, `present: 0-31`) it
/// declares 256 members for a 32-CPU machine. It is also `max_id + 1`, so a
/// gapped mask (`0-3,8-11`) declares the four ids in between as well.
///
/// Either way the extra slots are members of a DECLARED group, which is
/// exactly where `create_v3` skips the value-sentinel that would otherwise
/// have hidden them — so they are read out of the zero-filled BPF mmap and
/// published as a real `0`. That is the failure the declared-membership doc
/// warns about: "an over-declared prefix still publishes `0` for indices
/// nothing measured — a wrong value where the honest answer is no value at
/// all."
///
/// `present` rather than `online`: a CPU that is present but offline keeps its
/// slot and its identity, so its membership does not move when it is taken
/// down and brought back. `set_member_set` is a `OnceLock` — declared once at
/// init — so a membership that tracked `online` could not be updated on
/// hotplug anyway, and would silently be wrong after the first change.
///
/// Falls back to the dense prefix if the file is unreadable, which is the
/// behaviour this replaces rather than a new failure mode.
pub(crate) fn present_cpus() -> Vec<usize> {
    static CACHE: OnceLock<Vec<usize>> = OnceLock::new();
    CACHE
        .get_or_init(|| match crate::common::cpus() {
            Ok(cpus) if !cpus.is_empty() => cpus
                .into_iter()
                .filter(|c| *c < crate::agent::MAX_CPUS)
                .collect(),
            Ok(_) => {
                warn!(
                    "/sys/devices/system/cpu/present listed no CPUs; falling back to a \
                     dense 0..{} member set",
                    possible_cpus()
                );
                (0..possible_cpus()).collect()
            }
            Err(e) => {
                warn!(
                    "failed to read /sys/devices/system/cpu/present ({e}); falling back to \
                     a dense 0..{} member set, which over-declares on a host whose possible \
                     mask exceeds its present one",
                    possible_cpus()
                );
                (0..possible_cpus()).collect()
            }
        })
        .clone()
}

/// Parse the CPU count implied by `/sys/devices/system/cpu/possible`
/// syntax: a comma-separated list of individual ids and/or `lo-hi` ranges
/// (e.g. `"0-31"`, `"0"`, `"0-3,8-11"`). The file lists which ids the kernel
/// considers *possible* (a CPU that could be hot-added), not which are
/// currently online, so the answer is `max_id + 1` — the number of possible
/// slots — not a count of the listed ids, which may have gaps. Returns
/// `None` if the content is empty, doesn't parse as expected, or the
/// implied count overflows `usize` (`checked_add`, not `+1`: a garbage
/// mask like `"0-18446744073709551615"` must fall back, not wrap to 0 in
/// release), so the caller can fall back rather than trust a bogus bound.
///
/// Deliberately NOT clamped to `MAX_CPUS` here — that is a separate
/// concern ([`clamp_possible_cpus`]) so this function stays a pure parse of
/// what the file says, testable against arbitrarily large masks.
fn parse_possible_cpus(contents: &str) -> Option<usize> {
    let mut max_id: Option<usize> = None;

    for part in contents.trim().split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        let hi = match part.split_once('-') {
            Some((_, hi)) => hi.parse::<usize>().ok()?,
            None => part.parse::<usize>().ok()?,
        };

        max_id = Some(max_id.map_or(hi, |m| m.max(hi)));
    }

    max_id.and_then(|m| m.checked_add(1))
}

/// Clamp a parsed possible-CPU count to [`crate::agent::MAX_CPUS`].
///
/// Every per-CPU BPF counter map is sized for exactly `MAX_CPUS` slots —
/// `CounterMap::new`'s mmap region is `bank_cachelines * CACHELINE_SIZE *
/// MAX_CPUS` bytes, and every sampler's `mod.bpf.c` sizes `max_entries` the
/// same way — so a sweep bound above `MAX_CPUS` indexes
/// `counters[idx + cpu * bank_width]` off the end of the mapped slice. A
/// host whose `/sys/devices/system/cpu/possible` mask exceeds `MAX_CPUS`
/// (large `CONFIG_NR_CPUS`, hypervisor firmware reporting a big possible
/// range) is real, not hypothetical, so every migrated sampler's `refresh()`
/// would panic every tick without this clamp. This is the documented
/// silent-undercount tradeoff, not a crash: `docs/principles.md` principle
/// 6 already accepts "over-allocates on small machines, silently
/// under-counts past 1024 CPUs" as `MAX_CPUS`'s known ceiling; clamping
/// here is what actually delivers that promise for a possible-CPU sweep
/// bound instead of a fixed one.
fn clamp_possible_cpus(n: usize) -> usize {
    n.min(crate::agent::MAX_CPUS)
}

/// Number of possible CPUs on this host, per
/// `/sys/devices/system/cpu/possible` — POSSIBLE, deliberately not ONLINE,
/// so a CPU that comes up mid-recording was already counted and a sweep
/// bound taken at agent start does not miss it. Parsed once and cached,
/// then clamped to [`crate::agent::MAX_CPUS`] (see [`clamp_possible_cpus`])
/// so no caller can forget the clamp and index a per-CPU BPF map's mmap
/// region out of bounds. Falls back to `MAX_CPUS` if the file is missing,
/// empty, or fails to parse, so a sweep bound here degrades to the old
/// fixed bound rather than to zero.
pub(crate) fn possible_cpus() -> usize {
    static CACHE: OnceLock<usize> = OnceLock::new();
    *CACHE.get_or_init(|| {
        let contents = match std::fs::read_to_string("/sys/devices/system/cpu/possible") {
            Ok(contents) => contents,
            Err(e) => {
                debug!(
                    "failed to read /sys/devices/system/cpu/possible ({e}); \
                     falling back to MAX_CPUS={}",
                    crate::agent::MAX_CPUS
                );
                return crate::agent::MAX_CPUS;
            }
        };

        let Some(n) = parse_possible_cpus(&contents) else {
            warn!(
                "failed to parse /sys/devices/system/cpu/possible ({contents:?}); \
                 falling back to MAX_CPUS={}",
                crate::agent::MAX_CPUS
            );
            return crate::agent::MAX_CPUS;
        };

        let clamped = clamp_possible_cpus(n);
        if clamped != n {
            warn!(
                "possible CPU count {n} exceeds MAX_CPUS={}; per-CPU BPF counter maps are \
                 sized for MAX_CPUS, so clamping the sweep bound — CPUs beyond MAX_CPUS are \
                 silently excluded from per-CPU sweeps (docs/principles.md principle 6)",
                crate::agent::MAX_CPUS
            );
        }
        clamped
    })
}

pub fn process_cgroup_info<T>(data: &[u8], identity: &'static metriken::group::SlotIdentity) -> i32
where
    T: CgroupInfo + plain::Plain + Default,
{
    let mut cgroup_info = T::default();

    if plain::copy_from_bytes(&mut cgroup_info, data).is_ok() {
        let name = std::str::from_utf8(cgroup_info.name())
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let pname = std::str::from_utf8(cgroup_info.pname())
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        let gpname = std::str::from_utf8(cgroup_info.gpname())
            .unwrap_or("")
            .trim_end_matches(char::from(0))
            .replace("\\x2d", "-");

        // Construct hierarchical path based on level and available parent names
        let path = if name == "/" {
            // Root cgroup - just use "/"
            "/".to_string()
        } else if !gpname.is_empty() {
            if cgroup_info.level() > 3 {
                format!(".../{gpname}/{pname}/{name}")
            } else {
                format!("/{gpname}/{pname}/{name}")
            }
        } else if !pname.is_empty() {
            format!("/{pname}/{name}")
        } else if !name.is_empty() {
            format!("/{name}")
        } else {
            "".to_string()
        };

        // Written through `SlotIdentity`, which mints a new `__uid__` when the
        // name changes, so a renamed cgroup is a new series.
        if !path.is_empty() {
            identity.assign(
                cgroup_info.id() as usize,
                [("name".to_string(), path)].into_iter().collect(),
            );
        }
    }

    0
}

pub struct AsyncBpf {
    name: &'static str,
    thread: std::thread::JoinHandle<Result<(), libbpf_rs::Error>>,
    sync: SyncPrimitive,
    perf_threads: Vec<std::thread::JoinHandle<()>>,
    perf_sync: Vec<SyncPrimitive>,
    /// The declared [`AcquisitionGroup`] for this sampler's `.perf_event()`
    /// registrations, if it has any (see `Builder::perf_event`). `None` for
    /// every BPF sampler that never calls `.perf_event()` — the bracket
    /// below is then a no-op, same as before this field existed.
    perf_group: Option<&'static AcquisitionGroup>,
}

#[async_trait]
impl Sampler for AsyncBpf {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn refresh(&self) {
        if self.thread.is_finished() {
            panic!("{} bpf thread exited early", self.name);
        }

        self.sync.trigger();

        self.sync.wait_notify().await;

        for thread in self.perf_threads.iter() {
            if thread.is_finished() {
                panic!("{} perf thread exited early", self.name);
            }
        }

        // Brackets the perf-thread sweep only (not the BPF map refresh
        // above, which has its own per-map groups): `acquire()` before
        // triggering the thread(s), `finish()` only after `join_all`
        // confirms every thread has notified back — strictly after that
        // thread's `set()` calls (see `CpuPerfCounters::refresh`), so the
        // bracket spans the real read span (stamp-last, principle 18). A
        // single counter's failed/stalled perf read is individually,
        // normally fallible (see `classify_perf_read`), not a bulk sweep
        // failure, so there is no discard path here — same ruling as the
        // sampler-owned perf-thread samplers (`cpu_dtlb`, `cpu_l3`,
        // `cpu_frequency`, `cpu_branch`). A no-op (`perf_group` is `None`)
        // for every BPF sampler that never calls `.perf_event()`.
        let guard = self.perf_group.map(|g| g.acquire());

        let perf_futures: Vec<_> = self
            .perf_sync
            .iter()
            .map(|s| {
                s.trigger();
                s.wait_notify()
            })
            .collect();

        futures::future::join_all(perf_futures).await;

        if let Some(guard) = guard {
            guard.finish();
        }
    }
}

/// The gap between "how many slots could exist" and "which ids do exist" —
/// the distinction `present_cpus` was added for.
///
/// These are parse-level rather than filesystem-level: `present_cpus` reads
/// sysfs and caches in a `OnceLock`, so it cannot be driven from a test.
/// `crate::common::cpus` does the parsing it depends on, and these pin the
/// property that made the old dense bound wrong — that `possible_cpus`'
/// answer is not a member count.
#[cfg(test)]
mod present_vs_possible_tests {
    use super::parse_possible_cpus;

    /// A VM advertising hot-add capacity. The old code declared one member
    /// per possible SLOT, so a 32-CPU guest published 256 per-CPU series,
    /// 224 of them zeros read out of a zero-filled mmap.
    #[test]
    fn a_hotplug_capable_mask_implies_far_more_slots_than_cpus() {
        assert_eq!(parse_possible_cpus("0-255"), Some(256));
        // What the host actually has is a different file, and a different
        // answer: `present: 0-31` is 32 ids.
        let present: Vec<usize> = (0..=31).collect();
        assert_eq!(present.len(), 32);
        assert!(
            parse_possible_cpus("0-255").unwrap() > present.len(),
            "this is the over-declaration: 256 declared members, 32 real CPUs"
        );
    }

    /// A gapped mask. `possible_cpus` is documented to return `max_id + 1`
    /// precisely so a sweep bound covers the highest id — which means the
    /// gap is inside the range, and a dense bound declares it.
    #[test]
    fn a_gapped_mask_implies_slots_that_are_not_cpus() {
        assert_eq!(parse_possible_cpus("0-3,8-11"), Some(12));
        let present = [0usize, 1, 2, 3, 8, 9, 10, 11];
        assert_eq!(present.len(), 8);
        // Slots 4-7 are inside the bound and belong to no CPU.
        for phantom in 4..8 {
            assert!(!present.contains(&phantom));
            assert!(phantom < parse_possible_cpus("0-3,8-11").unwrap());
        }
    }

    /// The case where the two agree, which is every machine CI and `delta`
    /// run on — and why this went unnoticed.
    #[test]
    fn a_contiguous_fully_present_machine_hides_the_bug() {
        assert_eq!(parse_possible_cpus("0-31"), Some(32));
        let present: Vec<usize> = (0..=31).collect();
        assert_eq!(parse_possible_cpus("0-31").unwrap(), present.len());
    }
}

#[cfg(test)]
mod possible_cpus_tests {
    use super::{clamp_possible_cpus, parse_possible_cpus};

    #[test]
    fn parses_a_single_range() {
        assert_eq!(parse_possible_cpus("0-31"), Some(32));
    }

    #[test]
    fn parses_a_single_cpu() {
        assert_eq!(parse_possible_cpus("0"), Some(1));
    }

    #[test]
    fn parses_a_trailing_newline() {
        assert_eq!(parse_possible_cpus("0-31\n"), Some(32));
    }

    #[test]
    fn parses_a_list_of_ranges_and_singletons() {
        assert_eq!(parse_possible_cpus("0-3,8-11"), Some(12));
    }

    #[test]
    fn takes_the_max_id_across_unordered_parts() {
        // Not realistic content for this file, but the parser should not
        // assume the list arrives sorted.
        assert_eq!(parse_possible_cpus("8-11,0-3"), Some(12));
    }

    #[test]
    fn empty_content_is_unparseable() {
        assert_eq!(parse_possible_cpus(""), None);
        assert_eq!(parse_possible_cpus("\n"), None);
    }

    #[test]
    fn garbage_content_is_unparseable() {
        assert_eq!(parse_possible_cpus("not-a-cpu-list"), None);
    }

    #[test]
    fn parser_does_not_clamp_a_mask_beyond_max_cpus() {
        // The parser is NOT the bound: a possible mask bigger than
        // MAX_CPUS (1024) parses to its literal value. Clamping is
        // `clamp_possible_cpus`'s job, exercised separately below.
        assert_eq!(parse_possible_cpus("0-8191"), Some(8192));
    }

    #[test]
    fn overflowing_max_id_falls_back_to_unparseable_rather_than_wrapping() {
        // usize::MAX as the high end of a range: max_id + 1 must not wrap
        // to 0 (which would silently produce a zero-CPU sweep in release,
        // where integer overflow does not panic).
        assert_eq!(parse_possible_cpus("0-18446744073709551615"), None);
    }

    #[test]
    fn clamp_is_a_no_op_at_or_under_max_cpus() {
        assert_eq!(clamp_possible_cpus(1), 1);
        assert_eq!(clamp_possible_cpus(32), 32);
        assert_eq!(
            clamp_possible_cpus(crate::agent::MAX_CPUS),
            crate::agent::MAX_CPUS
        );
    }

    #[test]
    fn clamp_bounds_a_mask_beyond_max_cpus() {
        assert_eq!(clamp_possible_cpus(8192), crate::agent::MAX_CPUS);
        assert_eq!(clamp_possible_cpus(usize::MAX), crate::agent::MAX_CPUS);
    }
}
