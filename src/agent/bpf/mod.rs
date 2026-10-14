mod builder;
mod counters;
pub mod drivers;
pub mod filesystems;
mod histogram;
mod sync_primitive;

pub use builder::Builder as BpfBuilder;
pub use builder::{BpfProgStats, PerfEvent};

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

/// The programs a sampler loads, from what the kernel can run: the twins to
/// leave unloaded ([`BpfBuilder::disabled_programs`]), the (program,
/// capability) pairs required ([`BpfBuilder::required_programs`]), and the
/// (program, capability, reason) triples nothing replaces
/// ([`BpfBuilder::unavailable_programs`]).
#[derive(Debug, Default)]
pub struct ProgramSelection {
    pub disabled: Vec<&'static str>,
    pub required: Vec<(&'static str, &'static str)>,
    pub unavailable: Vec<(&'static str, &'static str, &'static str)>,
}

impl ProgramSelection {
    /// Whether `program` is required.
    #[cfg(test)]
    pub fn requires(&self, program: &str) -> bool {
        self.required.iter().any(|(p, _)| *p == program)
    }

    /// Whether `program` is reported as unavailable.
    #[cfg(test)]
    pub fn lacks(&self, program: &str) -> bool {
        self.unavailable.iter().any(|(p, _, _)| *p == program)
    }
}

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

/// Whether an `fentry`/`fexit` program can attach to kernel function `name` on
/// the running kernel: BTF describes it, the architecture can attach a
/// trampoline to a kernel function, and the function's argument count is
/// within the trampoline's limit. Callers pick the kprobe twin, or disable the
/// hook, when this is false.
///
/// [`kernel_has_btf`] answers a weaker question: "is there a vmlinux BTF at
/// all". libbpf resolves a trampoline program's `attach_btf_id` at LOAD time,
/// so a target missing from BTF is a load failure, which [`BpfBuilder`] treats
/// as fatal for the whole skeleton (`set_failed`, and `rezolus status` then
/// exits non-zero). A target that is in BTF but beyond the trampoline's reach
/// loads and fails at attach with `ENOTSUPP`, a failed required program. A
/// kprobe's miss is an ENOENT at attach, which is tolerated and reported as
/// unsupported. That matters for a hook behind a kernel config option:
/// `cpu_bandwidth`'s three targets are all inside `CONFIG_CFS_BANDWIDTH`.
///
/// Module BTF counts too: libbpf resolves an `fentry`/`fexit` target against
/// every loaded module's BTF (kernels 5.11+), so a function in `ext4.ko` is
/// attachable when `/sys/kernel/btf/ext4` describes it.
///
/// The argument count is BTF's parameter count; a struct passed by value takes more
/// than one register and is not accounted for.
///
/// - x86_64: up to 6 arguments before 6.6, 12 from 6.6.
/// - aarch64: up to 8 arguments (the 6.4–6.15 limit, kept as the floor), and
///   only with
///   `CONFIG_DYNAMIC_FTRACE_WITH_DIRECT_CALLS`, which arm64 first offers in
///   6.4 and which `CFI_CLANG`, or a size-optimized GCC build (any
///   size-optimized build before 6.9), rules out. Read from
///   `/boot/config-<release>` when that file exists, else taken as release
///   6.4 or later; an agent in a container usually has no `/boot`, and gets
///   the release rule.
///
/// A release that cannot be read or parsed is taken as older than either
/// limit, since the kprobe twin attaches everywhere.
pub fn kernel_can_fentry(name: &str) -> bool {
    let Some(args) = kernel_btf_func_arg_count(name) else {
        return false;
    };

    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let config = std::fs::read_to_string(format!("/boot/config-{}", release.trim())).ok();

    can_fentry(std::env::consts::ARCH, &release, config.as_deref(), args)
}

/// [`kernel_can_fentry`] for every one of `names`: for a sampler whose fentry
/// twins are chosen together.
pub fn kernel_can_fentry_all(names: &[&str]) -> bool {
    let mut all = true;

    // Every name is checked, not just up to the first miss, so the debug log
    // names each function that sends the sampler to its kprobes.
    for name in names {
        if !kernel_can_fentry(name) {
            debug!("fentry cannot attach to `{name}` on this kernel");
            all = false;
        }
    }

    all
}

/// For hooks given as `(function, fentry program, kprobe program)`, the twin
/// of each to disable: the kprobe where [`kernel_can_fentry`] holds for the
/// function, else the fentry program.
pub fn unused_twins(hooks: &[(&str, &'static str, &'static str)]) -> Vec<&'static str> {
    hooks
        .iter()
        .map(|(function, fentry, kprobe)| {
            if kernel_can_fentry(function) {
                *kprobe
            } else {
                *fentry
            }
        })
        .collect()
}

fn can_fentry(arch: &str, release: &str, config: Option<&str>, args: u32) -> bool {
    let version = release_version(release);

    match arch {
        "x86_64" => {
            let limit = if version.is_some_and(|v| v >= (6, 6)) {
                12
            } else {
                6
            };
            args <= limit
        }
        "aarch64" => {
            let direct_calls = match config {
                Some(config) => config
                    .lines()
                    .any(|line| line.trim() == "CONFIG_DYNAMIC_FTRACE_WITH_DIRECT_CALLS=y"),
                None => version.is_some_and(|v| v >= (6, 4)),
            };
            direct_calls && args <= 8
        }
        _ => true,
    }
}

/// `(major, minor)` from a kernel release string such as `6.12.75+deb13-arm64`
/// or `6.4-rc1`, or `None` when it does not start with two numbers.
fn release_version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.trim().split(['.', '-', '+']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
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
    with_kernel_btf(|btf| btf.find(|index| index.funcs.get(name).copied()))?.flatten()
}

/// Returns true if the running kernel's BTF — vmlinux **or any loaded
/// module's** — describes every one of `names` as a tracepoint, i.e. carries
/// its `btf_trace_<name>` typedef, so a `tp_btf` program may name it as an
/// attach target.
///
/// [`kernel_can_fentry`] answers the same question for `fentry`/`fexit`
/// targets. Module BTF matters for both: on a kernel with `CONFIG_EXT4_FS=m`
/// (stock Debian amd64, for one), `btf_trace_jbd2_run_stats` is in
/// `/sys/kernel/btf/jbd2`, not in vmlinux, and libbpf does resolve `tp_btf`
/// targets against module BTF (kernels 5.11+). Selecting the `tp_btf`
/// twin on [`kernel_has_btf`] alone would make that kernel a load failure,
/// fatal for the whole skeleton (see [`kernel_can_fentry`] for why load-time
/// misses matter more than attach-time ones). Selecting on this function falls
/// back to the `raw_tp` twin, whose attach failure on a kernel without the
/// tracepoint at all is an ENOENT the builder tolerates.
///
/// Returns false when there is no kernel BTF to consult.
pub fn kernel_btf_has_tracepoints(names: &[&str]) -> bool {
    with_kernel_btf(|btf| {
        let mut all = true;

        for name in names {
            if btf
                .find(|index| index.tracepoints.contains_key(*name).then_some(()))
                .is_none()
            {
                debug!("kernel BTF (vmlinux and modules) has no tracepoint `{name}`");
                all = false;
            }
        }

        all
    })
    .unwrap_or(false)
}

/// Whether the running kernel defines the BPF helper `name` (for example
/// `bpf_skc_to_tcp_sock`), from its BTF.
///
/// The kernel defines each helper with a `BPF_CALL_<n>` macro, which emits the
/// typedef `btf_<name>`; it is in vmlinux BTF exactly when the helper is,
/// including on distro kernels that backported it. Whether a given program
/// type may call it is a separate question: callers name a helper whose
/// availability to their program type arrived with the helper itself.
///
/// libbpf's helper probe cannot answer this for tracing programs: it cannot
/// load a standalone `BPF_PROG_TYPE_TRACING` probe, and the `kprobe` probe
/// that `probe_task_storage_helper` uses instead only sees helpers in the
/// table `kprobe` programs share with tracing programs. Helpers offered only
/// in `tracing_prog_func_proto`, such as `bpf_skc_to_tcp_sock`, read as
/// absent through it. A kernel whose BTF lacks the typedef reads as
/// unsupported, which selects the caller's fallback, never a load failure.
/// Returns false when there is no kernel BTF to consult.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn kernel_btf_has_helper(name: &str) -> bool {
    let found = with_kernel_btf(|btf| btf.vmlinux.helpers.contains(name)).unwrap_or(false);

    if !found {
        debug!("kernel BTF has no BPF helper `{name}`");
    }

    found
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
    with_kernel_btf(|btf| btf.find(|index| index.tracepoints.get(name).copied()))?.flatten()
}

/// The kernel BTF index the queries above share, built on first use.
static KERNEL_BTF: std::sync::Mutex<Option<KernelBtf>> = std::sync::Mutex::new(None);

/// Runs `f` on the kernel's BTF index, first indexing vmlinux if this is the
/// first query and any module BTF that appeared since the last one. `None`
/// when the kernel has no BTF or vmlinux's does not parse.
///
/// Module BTF is listed again on every call because a module can load after
/// the agent starts: a deferred sampler (`xfs_log`, `ext4_ops`) initialises
/// once its filesystem module has loaded and asks about functions in it.
fn with_kernel_btf<T>(f: impl FnOnce(&KernelBtf) -> T) -> Option<T> {
    if !kernel_has_btf() {
        return None;
    }

    let mut guard = KERNEL_BTF.lock().unwrap_or_else(|e| e.into_inner());

    if guard.is_none() {
        let vmlinux = RawBtf::parse(Path::new("/sys/kernel/btf/vmlinux"), None)?;
        *guard = Some(KernelBtf {
            vmlinux: BtfIndex::build(&vmlinux),
            modules: HashMap::new(),
            loaded: Vec::new(),
        });
    }

    let btf = guard.as_mut()?;
    btf.refresh(
        Path::new("/sys/kernel/btf/vmlinux"),
        Path::new("/sys/kernel/btf"),
    );

    Some(f(btf))
}

/// The indexed BTF of vmlinux and of every module seen so far.
struct KernelBtf {
    vmlinux: BtfIndex,
    /// By module name; `None` for a module whose BTF did not parse, so it is
    /// not parsed again on every query.
    modules: HashMap<std::ffi::OsString, Option<BtfIndex>>,
    /// The modules listed under `/sys/kernel/btf` at the last refresh. A
    /// module that has since unloaded keeps its index in `modules` but is not
    /// consulted.
    loaded: Vec<std::ffi::OsString>,
}

impl KernelBtf {
    /// Lists the module BTF in `dir` and indexes each module not yet seen.
    /// vmlinux is parsed again, as the base of their split BTF, only when
    /// there is such a module.
    fn refresh(&mut self, vmlinux: &Path, dir: &Path) {
        let paths = module_btf_paths(dir);

        let unseen: Vec<&std::path::PathBuf> = paths
            .iter()
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| !self.modules.contains_key(name))
            })
            .collect();

        if !unseen.is_empty() {
            if let Some(base) = RawBtf::parse(vmlinux, None) {
                for path in unseen {
                    let index = RawBtf::parse(path, Some(&base)).map(|btf| BtfIndex::build(&btf));
                    if let Some(name) = path.file_name() {
                        self.modules.insert(name.to_owned(), index);
                    }
                }
            }
        }

        self.loaded = paths
            .iter()
            .filter_map(|path| path.file_name().map(|name| name.to_owned()))
            .collect();
    }

    /// The first answer `get` gives, from vmlinux and then from each loaded
    /// module. vmlinux comes first because a name it defines is the one
    /// libbpf resolves.
    fn find<T>(&self, get: impl Fn(&BtfIndex) -> Option<T>) -> Option<T> {
        get(&self.vmlinux).or_else(|| {
            self.loaded
                .iter()
                .filter_map(|name| self.modules.get(name)?.as_ref())
                .find_map(get)
        })
    }
}

/// What the queries above read from one BTF object, read in a single pass
/// over its types. `btf__find_by_name_kind` scans from type id 1 on every
/// call, and on a module's split BTF that scan includes all of vmlinux, so
/// answering each query from libbpf cost 1.1 s to 1.5 s of agent startup on
/// Debian 12 and 13 with about 85 modules; reading each object once into maps
/// replaces those scans with hash lookups.
///
/// When a name occurs more than once (static functions in different
/// translation units), the entry is the one with the lowest type id, which is
/// the one `btf__find_by_name_kind` returns.
#[derive(Default)]
struct BtfIndex {
    /// Function name to parameter count; `None` when the function's type is
    /// not a well-formed prototype.
    funcs: HashMap<Box<str>, Option<u32>>,
    /// Tracepoint name (its `btf_trace_<name>` typedef, prefix removed) to
    /// the number of arguments it passes; `None` when the typedef is not
    /// shaped as a tracepoint's.
    tracepoints: HashMap<Box<str>, Option<u32>>,
    /// Names `<name>` for which the BTF has a typedef `btf_<name>`, which
    /// `BPF_CALL_<n>` emits for each BPF helper. Tracepoint typedefs are left
    /// out.
    helpers: HashSet<Box<str>>,
}

impl BtfIndex {
    /// Indexes the types `btf` itself defines: all of them for vmlinux, and
    /// for a module's split BTF only those after its base's, since the
    /// base's are in the vmlinux index.
    fn build(btf: &RawBtf) -> Self {
        let mut index = Self::default();

        // SAFETY: `btf.0` is a live BTF object, as is its base when it has
        // one; the ids passed to `btf__type_by_id` are below its type count,
        // and the name pointers are read before `btf` is freed.
        unsafe {
            let ptr = btf.0.as_ptr();
            let base = libbpf_sys::btf__base_btf(ptr);
            let first = if base.is_null() {
                1
            } else {
                libbpf_sys::btf__type_cnt(base)
            };

            for id in first..libbpf_sys::btf__type_cnt(ptr) {
                let ty = libbpf_sys::btf__type_by_id(ptr, id);
                if ty.is_null() {
                    continue;
                }

                let kind = btf_kind(&*ty);
                if kind != libbpf_sys::BTF_KIND_FUNC && kind != libbpf_sys::BTF_KIND_TYPEDEF {
                    continue;
                }

                let name = libbpf_sys::btf__name_by_offset(ptr, (*ty).name_off);
                if name.is_null() {
                    continue;
                }
                let Ok(name) = std::ffi::CStr::from_ptr(name).to_str() else {
                    continue;
                };

                if kind == libbpf_sys::BTF_KIND_FUNC {
                    if !index.funcs.contains_key(name) {
                        index.funcs.insert(name.into(), btf.func_arg_count_of(id));
                    }
                } else if let Some(tracepoint) = name.strip_prefix("btf_trace_") {
                    if !index.tracepoints.contains_key(tracepoint) {
                        index
                            .tracepoints
                            .insert(tracepoint.into(), btf.tracepoint_arg_count_of(id));
                    }
                } else if let Some(helper) = name.strip_prefix("btf_") {
                    index.helpers.insert(helper.into());
                }
            }
        }

        index
    }
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

    /// The id of the type named `name` of `kind` in this BTF, base included,
    /// or `None`. The tests check [`BtfIndex`] against this.
    #[cfg(test)]
    fn find(&self, name: &str, kind: u32) -> Option<u32> {
        let cname = std::ffi::CString::new(name).ok()?;

        // SAFETY: `self.0` is a live BTF object and `cname` a valid C string.
        let id =
            unsafe { libbpf_sys::btf__find_by_name_kind(self.0.as_ptr(), cname.as_ptr(), kind) };
        u32::try_from(id).ok()
    }

    /// [`func_arg_count_of`](Self::func_arg_count_of) for the function named
    /// `name`, found by libbpf.
    #[cfg(test)]
    fn func_arg_count(&self, name: &str) -> Option<u32> {
        self.func_arg_count_of(self.find(name, libbpf_sys::BTF_KIND_FUNC)?)
    }

    /// [`tracepoint_arg_count_of`](Self::tracepoint_arg_count_of) for the
    /// tracepoint named `name`, found by libbpf.
    #[cfg(test)]
    fn tracepoint_arg_count(&self, name: &str) -> Option<u32> {
        self.tracepoint_arg_count_of(
            self.find(&format!("btf_trace_{name}"), libbpf_sys::BTF_KIND_TYPEDEF)?,
        )
    }

    /// The number of parameters of the function with type id `id`, from its
    /// BTF: a `FUNC` refers to a `FUNC_PROTO` whose vlen is the parameter
    /// count. `None` if `id` is not a `FUNC` with such a prototype.
    fn func_arg_count_of(&self, id: u32) -> Option<u32> {
        // SAFETY: `self.0` is a live BTF object, `btf__type_by_id` returns null
        // for an id it does not have, and the returned pointer is read before
        // the object is freed.
        unsafe {
            let func = libbpf_sys::btf__type_by_id(self.0.as_ptr(), id);
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

    /// The number of arguments a tracepoint passes to a `tp_btf` or `raw_tp`
    /// program, from its `btf_trace_<name>` typedef with type id `id`: the
    /// typedef names a pointer to a function prototype whose first parameter
    /// is the tracepoint's `void *` context and whose remaining parameters
    /// are the `TP_PROTO` arguments, in order. `None` if the typedef is not
    /// shaped that way.
    fn tracepoint_arg_count_of(&self, id: u32) -> Option<u32> {
        // SAFETY: `self.0` is a live BTF object, `btf__type_by_id` returns null
        // for an id it does not have, and the returned pointers are read
        // before the object is freed.
        unsafe {
            // typedef -> ptr -> func_proto; `type` on the first two is the
            // referenced type id, and vlen on the last is the parameter count.
            let mut ty = libbpf_sys::btf__type_by_id(self.0.as_ptr(), id);
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

impl Drop for RawBtf {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `btf__parse`/`btf__parse_split` and is
        // freed exactly once, here.
        unsafe { libbpf_sys::btf__free(self.0.as_ptr()) }
    }
}

#[cfg(test)]
mod btf_tests {
    use super::{btf_kind, module_btf_paths, BtfIndex, KernelBtf, RawBtf};
    use std::collections::HashMap;
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

    fn index(funcs: &[(&str, u32)]) -> BtfIndex {
        BtfIndex {
            funcs: funcs
                .iter()
                .map(|(name, n)| ((*name).into(), Some(*n)))
                .collect(),
            ..Default::default()
        }
    }

    /// vmlinux answers before any module, and a module that has unloaded
    /// since it was indexed is not consulted.
    #[test]
    fn find_prefers_vmlinux_and_skips_unloaded_modules() {
        let btf = KernelBtf {
            vmlinux: index(&[("shared", 2)]),
            modules: HashMap::from([
                (
                    "loaded".into(),
                    Some(index(&[("shared", 7), ("in_loaded", 3)])),
                ),
                ("unloaded".into(), Some(index(&[("in_unloaded", 5)]))),
                ("unparsed".into(), None),
            ]),
            loaded: vec!["unparsed".into(), "loaded".into()],
        };
        let arg_count = |name: &str| btf.find(|i| i.funcs.get(name).copied()).flatten();

        assert_eq!(arg_count("shared"), Some(2));
        assert_eq!(arg_count("in_loaded"), Some(3));
        assert_eq!(arg_count("in_unloaded"), None);
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
        let index = BtfIndex::build(&vmlinux);
        // sched_switch has passed 3 (through 5.13) or 4 arguments (5.14+,
        // `prev_state` added); any other count means the walk is wrong.
        let n = index.tracepoints["sched_switch"].expect("sched_switch is a tracepoint");
        assert!((3..=4).contains(&n), "sched_switch has {n} arguments");
        assert!(!index.tracepoints.contains_key("no_such_tracepoint"));
        // vfs_fsync_range(struct file *, loff_t, loff_t, int) on every kernel
        // since 2.6; the count is the FUNC_PROTO's vlen, not vlen - 1 as for a
        // tracepoint's typedef (no leading context pointer).
        assert_eq!(index.funcs["vfs_fsync_range"], Some(4));
        assert!(!index.funcs.contains_key("no_such_function"));
        // BPF_CALL_2(bpf_map_lookup_elem, ...) has been in every kernel with BPF.
        assert!(index.helpers.contains("bpf_map_lookup_elem"));
        for path in module_btf_paths(Path::new("/sys/kernel/btf")) {
            assert!(
                RawBtf::parse(&path, Some(&vmlinux)).is_some(),
                "{} did not parse as split BTF",
                path.display()
            );
        }
    }

    /// The index gives libbpf's answer: for a sample of the functions and
    /// tracepoints in vmlinux and in each module, the arity looked up by
    /// `btf__find_by_name_kind` equals what `KernelBtf::find` returns. A
    /// module's libbpf lookup walks vmlinux first, as `find` does. Skipped
    /// where there is no kernel BTF.
    #[test]
    fn the_index_agrees_with_libbpf_lookups() {
        let vmlinux_path = Path::new("/sys/kernel/btf/vmlinux");
        if !vmlinux_path.exists() {
            return;
        }
        let vmlinux = RawBtf::parse(vmlinux_path, None).expect("vmlinux BTF parses");
        let mut btf = KernelBtf {
            vmlinux: BtfIndex::build(&vmlinux),
            modules: HashMap::new(),
            loaded: Vec::new(),
        };
        btf.refresh(vmlinux_path, Path::new("/sys/kernel/btf"));

        let mut checked = 0;
        let mut check = |raw: &RawBtf, index: &BtfIndex, step: usize| {
            for name in index.funcs.keys().step_by(step) {
                let indexed = btf.find(|i| i.funcs.get(&**name).copied()).flatten();
                assert_eq!(indexed, raw.func_arg_count(name), "function {name}");
                checked += 1;
            }
            for name in index.tracepoints.keys().step_by(step) {
                let indexed = btf.find(|i| i.tracepoints.get(&**name).copied()).flatten();
                assert_eq!(indexed, raw.tracepoint_arg_count(name), "tracepoint {name}");
                checked += 1;
            }
        };

        check(&vmlinux, &btf.vmlinux, 97);
        for path in module_btf_paths(Path::new("/sys/kernel/btf")) {
            let raw = RawBtf::parse(&path, Some(&vmlinux)).expect("module BTF parses");
            check(&raw, &BtfIndex::build(&raw), 13);
        }
        assert!(checked > 0);

        // Every function vmlinux defines more than once with different
        // arities (static functions in different translation units), where
        // the index must keep the one libbpf finds: the lowest id.
        let mut first: HashMap<String, Option<u32>> = HashMap::new();
        let mut ambiguous = Vec::new();
        // SAFETY: `vmlinux.0` is a live BTF object, every id is below its type
        // count, and the name pointers are read before it is freed.
        unsafe {
            let ptr = vmlinux.0.as_ptr();
            for id in 1..libbpf_sys::btf__type_cnt(ptr) {
                let ty = libbpf_sys::btf__type_by_id(ptr, id);
                if ty.is_null() || btf_kind(&*ty) != libbpf_sys::BTF_KIND_FUNC {
                    continue;
                }
                let name =
                    std::ffi::CStr::from_ptr(libbpf_sys::btf__name_by_offset(ptr, (*ty).name_off))
                        .to_string_lossy()
                        .into_owned();
                let n = vmlinux.func_arg_count_of(id);
                match first.get(&name) {
                    Some(m) if *m != n => ambiguous.push(name),
                    Some(_) => {}
                    None => {
                        first.insert(name, n);
                    }
                }
            }
        }
        eprintln!("{} ambiguous vmlinux function names", ambiguous.len());
        for name in &ambiguous {
            assert_eq!(
                btf.vmlinux.funcs[name.as_str()],
                vmlinux.func_arg_count(name),
                "function {name}"
            );
        }
    }

    /// A module whose BTF appears after the first query is indexed by the
    /// next refresh, and one that disappears is no longer listed. Uses links
    /// to the running kernel's module BTF in a scratch directory; skipped
    /// where there is none.
    #[test]
    fn refresh_follows_modules_that_load_and_unload() {
        let vmlinux_path = Path::new("/sys/kernel/btf/vmlinux");
        let Some(module) = module_btf_paths(Path::new("/sys/kernel/btf"))
            .into_iter()
            .next()
        else {
            return;
        };
        let vmlinux = RawBtf::parse(vmlinux_path, None).expect("vmlinux BTF parses");
        let name = module.file_name().unwrap().to_owned();

        let dir = std::env::temp_dir().join(format!("rezolus-btf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut btf = KernelBtf {
            vmlinux: BtfIndex::build(&vmlinux),
            modules: HashMap::new(),
            loaded: Vec::new(),
        };
        btf.refresh(vmlinux_path, &dir);
        assert!(btf.loaded.is_empty());

        std::os::unix::fs::symlink(&module, dir.join(&name)).unwrap();
        btf.refresh(vmlinux_path, &dir);
        assert_eq!(btf.loaded, vec![name.clone()]);
        assert!(matches!(btf.modules.get(&name), Some(Some(_))));

        std::fs::remove_file(dir.join(&name)).unwrap();
        btf.refresh(vmlinux_path, &dir);
        assert!(btf.loaded.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
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
/// Whether tracing programs can call `bpf_task_pt_regs` (5.15+): the saved
/// register set of a task, from which `memory_pagecache` reads the syscall
/// number that classifies a page-cache fill. Probed as task storage is,
/// through the `kprobe` program type (see `probe_task_storage_helper`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn kernel_tracing_has_task_pt_regs() -> bool {
    // SAFETY: FFI call with plain enum arguments and a null options pointer,
    // which libbpf documents as "default options".
    let ret = unsafe {
        libbpf_sys::libbpf_probe_bpf_helper(
            libbpf_sys::BPF_PROG_TYPE_KPROBE,
            libbpf_sys::BPF_FUNC_task_pt_regs,
            std::ptr::null(),
        )
    };
    if ret != 1 {
        debug!("kernel BPF helper probe for bpf_task_pt_regs from tracing programs returned {ret}");
    }
    ret == 1
}

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
/// For BPF programs whose tracepoint arguments are in jiffies (the writeback
/// throttle's `pause`): hand this to the program through
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

use counters::{CgroupCounters, Counters, CpuCounters, FilesystemCounters, PackedCounters};
use histogram::{Histogram, HistogramBatch, HISTOGRAM_SLICES};
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
                warn!(
                    "failed to read /sys/devices/system/cpu/possible ({e}); \
                     falling back to MAX_CPUS={}, which sizes every BPF histogram \
                     for that many CPUs (about 3.9 MiB each)",
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

/// Entries in the CPU-to-copy table of per-cgroup counter maps,
/// `cgroup_shard_of_cpu` in `bpf/cgroup.h`. A CPU id at or above it uses
/// entry `cpu % CGROUP_SHARD_TABLE`.
pub(crate) const CGROUP_SHARD_TABLE: usize = 1024;

/// The CPU-to-copy table for per-cgroup counter maps (`cgroup_shard_of_cpu`
/// in `bpf/cgroup.h`), read once from sysfs. See [`assign_cgroup_shards`].
pub(crate) fn cgroup_shard_table() -> &'static [u8; CGROUP_SHARD_TABLE] {
    static TABLE: OnceLock<[u8; CGROUP_SHARD_TABLE]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let cpus: Vec<(usize, usize)> = (0..possible_cpus())
            .filter_map(|cpu| l3_group(cpu).map(|group| (cpu, group)))
            .collect();
        if cpus.is_empty() {
            debug!("no L3 cache topology in sysfs; per-cgroup counters use cpu % CGROUP_SHARDS");
        }
        assign_cgroup_shards(&cpus)
    })
}

/// The lowest CPU id sharing `cpu`'s L3 cache, from sysfs, or `cpu` itself
/// when the CPU has cache information but no L3. `None` when sysfs has no
/// cache information for the CPU (offline, or not exposed).
fn l3_group(cpu: usize) -> Option<usize> {
    let base = format!("/sys/devices/system/cpu/cpu{cpu}/cache");
    let mut found = false;
    for index in 0.. {
        let dir = format!("{base}/index{index}");
        let Ok(level) = std::fs::read_to_string(format!("{dir}/level")) else {
            break;
        };
        found = true;
        if level.trim() != "3" {
            continue;
        }
        let list = std::fs::read_to_string(format!("{dir}/shared_cpu_list")).ok()?;
        return list
            .trim()
            .split([',', '-'])
            .next()
            .and_then(|first| first.parse().ok());
    }
    found.then_some(cpu)
}

/// Each listed CPU's entry in the CPU-to-copy table: 1 + its copy. The CPUs
/// are given as `(cpu, l3 group)`, and no copy is shared across two L3
/// caches unless there are more L3 caches than copies:
///
/// - With at least `CGROUP_SHARDS` L3 caches, each copy takes whole caches,
///   in order of group.
/// - With fewer, the copies are divided among the caches in proportion to
///   their CPUs, at least one each and at most one per CPU, and each cache's
///   CPUs are divided among its copies in id order.
///
/// An unlisted CPU's entry is 0, which the BPF side reads as copy
/// `cpu % CGROUP_SHARDS`.
pub(crate) fn assign_cgroup_shards(cpus: &[(usize, usize)]) -> [u8; CGROUP_SHARD_TABLE] {
    let shards = crate::agent::CGROUP_SHARDS;
    let mut table = [0u8; CGROUP_SHARD_TABLE];

    let mut by_group: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for &(cpu, group) in cpus {
        by_group.entry(group).or_default().push(cpu);
    }
    let groups: Vec<Vec<usize>> = by_group
        .into_values()
        .map(|mut members| {
            members.sort_unstable();
            members
        })
        .collect();
    if groups.is_empty() {
        return table;
    }

    let mut set = |cpu: usize, shard: usize| table[cpu % CGROUP_SHARD_TABLE] = shard as u8 + 1;

    if groups.len() >= shards {
        for (j, members) in groups.iter().enumerate() {
            for &cpu in members {
                set(cpu, j * shards / groups.len());
            }
        }
        return table;
    }

    // Copies per group: the floor of its share, clamped to [1, its CPUs],
    // then moved one at a time to the group with the most CPUs per copy (or
    // from the group with the fewest) until they add up to `shards` or no
    // group can take another.
    let total: usize = groups.iter().map(Vec::len).sum();
    let mut counts: Vec<usize> = groups
        .iter()
        .map(|members| (members.len() * shards / total).clamp(1, members.len()))
        .collect();
    loop {
        let sum: usize = counts.iter().sum();
        let pick = if sum < shards {
            (0..groups.len())
                .filter(|&i| counts[i] < groups[i].len())
                .max_by(|&a, &b| (groups[a].len() * counts[b]).cmp(&(groups[b].len() * counts[a])))
                .map(|i| (i, true))
        } else if sum > shards {
            (0..groups.len())
                .filter(|&i| counts[i] > 1)
                .min_by(|&a, &b| (groups[a].len() * counts[b]).cmp(&(groups[b].len() * counts[a])))
                .map(|i| (i, false))
        } else {
            None
        };
        match pick {
            Some((i, true)) => counts[i] += 1,
            Some((i, false)) => counts[i] -= 1,
            None => break,
        }
    }

    let mut base = 0;
    for (members, &count) in groups.iter().zip(&counts) {
        for (i, &cpu) in members.iter().enumerate() {
            set(cpu, base + i * count / members.len());
        }
        base += count;
    }
    table
}

#[cfg(test)]
mod cgroup_shard_table_tests {
    use super::*;
    use crate::agent::CGROUP_SHARDS;

    fn copies(table: &[u8; CGROUP_SHARD_TABLE], cpus: impl Iterator<Item = usize>) -> Vec<u8> {
        cpus.map(|cpu| table[cpu]).collect()
    }

    /// A 32-core host with SMT, siblings numbered 32-63, and eight L3 caches
    /// of four cores: each copy is written by the four threads of one L3
    /// cache's first or second pair of cores, never by two L3 caches.
    #[test]
    fn copies_stay_within_an_l3_cache() {
        let cpus: Vec<(usize, usize)> = (0..64).map(|cpu| (cpu, (cpu % 32) / 4 * 4)).collect();
        let table = assign_cgroup_shards(&cpus);
        for shard in 1..=CGROUP_SHARDS as u8 {
            let writers: Vec<usize> = (0..64).filter(|&cpu| table[cpu] == shard).collect();
            assert_eq!(writers.len(), 4, "copy {shard}: {writers:?}");
            let groups: std::collections::HashSet<usize> =
                writers.iter().map(|cpu| (cpu % 32) / 4).collect();
            assert_eq!(groups.len(), 1, "copy {shard}: {writers:?}");
        }
    }

    /// The hv01 guest's shape, seven L3 caches of eight consecutive CPUs:
    /// all 16 copies are used and none is shared across two caches.
    #[test]
    fn copies_do_not_straddle_l3_caches() {
        let cpus: Vec<(usize, usize)> = (0..56).map(|cpu| (cpu, cpu / 8 * 8)).collect();
        let table = assign_cgroup_shards(&cpus);
        for shard in 1..=CGROUP_SHARDS as u8 {
            let writers: Vec<usize> = (0..56).filter(|&cpu| table[cpu] == shard).collect();
            assert!(
                (2..=4).contains(&writers.len()),
                "copy {shard}: {writers:?}"
            );
            assert!(
                writers.iter().all(|cpu| cpu / 8 == writers[0] / 8),
                "copy {shard}: {writers:?}"
            );
        }
    }

    /// More L3 caches than copies: each copy takes whole caches.
    #[test]
    fn many_l3_caches_share_copies_whole() {
        let cpus: Vec<(usize, usize)> = (0..192).map(|cpu| (cpu, cpu / 8)).collect();
        let table = assign_cgroup_shards(&cpus);
        for group in 0..24 {
            let copies: std::collections::HashSet<u8> =
                (group * 8..group * 8 + 8).map(|cpu| table[cpu]).collect();
            assert_eq!(copies.len(), 1, "cache {group}");
        }
        let used: std::collections::HashSet<u8> = table[..192].iter().copied().collect();
        assert_eq!(used.len(), CGROUP_SHARDS);
    }

    /// With no more CPUs than copies, every CPU has a copy of its own.
    #[test]
    fn small_hosts_get_a_copy_per_cpu() {
        let cpus: Vec<(usize, usize)> = (0..8).map(|cpu| (cpu, 0)).collect();
        let table = assign_cgroup_shards(&cpus);
        let mut seen = copies(&table, 0..8);
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 8);
        assert!(!seen.contains(&0));
    }

    /// Every listed CPU gets a copy in 1..=CGROUP_SHARDS, unlisted ones 0, and
    /// a large host uses every copy.
    #[test]
    fn entries_are_in_range() {
        let cpus: Vec<(usize, usize)> = (0..192)
            .filter(|c| c % 7 != 3)
            .map(|c| (c, c / 16))
            .collect();
        let table = assign_cgroup_shards(&cpus);
        for (cpu, &entry) in table.iter().enumerate().take(192) {
            if cpu % 7 == 3 {
                assert_eq!(entry, 0);
            } else {
                assert!((1..=CGROUP_SHARDS as u8).contains(&entry));
            }
        }
        let mut used = copies(&table, (0..192).filter(|c| c % 7 != 3));
        used.sort();
        used.dedup();
        assert_eq!(used.len(), CGROUP_SHARDS);
    }

    /// `cgroup.h` declares the table with the same length.
    #[test]
    fn bpf_header_has_the_same_table() {
        let header = include_str!("cgroup.h");
        assert!(header.contains(&format!(
            "#define CGROUP_SHARD_TABLE {CGROUP_SHARD_TABLE}\n"
        )));
        assert!(header.contains("const volatile u8 cgroup_shard_of_cpu[CGROUP_SHARD_TABLE] = {};"));
    }
}

/// The multiplier in `pid_slot`, and in `pid_slot()` in
/// `src/agent/bpf/helpers.h`.
const PID_SLOT_MULTIPLIER: usize = 277;

/// The index of `pid` in a per-pid BPF array, the same mapping as
/// `pid_slot()` in `src/agent/bpf/helpers.h`: the pid's offset within its
/// block of 512 is multiplied by 277 mod 512, so 48 consecutive pids write
/// 48 different cache lines (`helpers.h` gives the line counts at other
/// strides). One to one, and every pid stays in its own 512-entry block.
pub fn pid_slot(pid: usize) -> usize {
    (pid & !511) | (pid.wrapping_mul(PID_SLOT_MULTIPLIER) & 511)
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

#[cfg(test)]
mod pid_slot_tests {
    use super::{pid_slot, PID_SLOT_MULTIPLIER};

    /// Every pid below the kernel's PID_MAX_LIMIT (2^22) has its own slot,
    /// in the same 512-entry block (4 KiB page of u64s) as the pid.
    #[test]
    fn one_to_one_within_each_block() {
        let n = 1usize << 22;
        let mut seen = vec![false; n];
        for pid in 0..n {
            let slot = pid_slot(pid);
            assert_eq!(slot / 512, pid / 512, "pid {pid} -> {slot}");
            assert!(!seen[slot], "pid {pid} shares slot {slot}");
            seen[slot] = true;
        }
    }

    /// 48 threads whose pids are consecutive, or spaced by a power of two up
    /// to 64, write 48 different cache lines of 8 entries, from any start.
    /// Indexed by pid, 48 consecutive pids share 6 or 7 lines.
    #[test]
    fn threads_at_common_strides_use_different_lines() {
        for stride in [1, 2, 4, 8, 16, 32, 64] {
            for start in 0..1024 {
                let lines: std::collections::HashSet<usize> =
                    (0..48).map(|k| pid_slot(start + k * stride) / 8).collect();
                assert_eq!(lines.len(), 48, "stride {stride} start {start}");
            }
        }
    }

    /// The BPF helper in helpers.h computes the same expression with the
    /// same multiplier. It is not compiled for the host, so its source is
    /// checked here: a change to the helper fails this test until
    /// `PID_SLOT_MULTIPLIER` matches it.
    #[test]
    fn bpf_helper_is_the_same_expression() {
        let helpers = include_str!("helpers.h");
        let expr = format!("return (pid & ~511U) | ((pid * {PID_SLOT_MULTIPLIER}) & 511);");
        assert!(helpers.contains(&expr), "helpers.h does not contain {expr}");
    }

    /// Fixed values, so a change to this function shows up.
    #[test]
    fn known_values() {
        assert_eq!(pid_slot(0), 0);
        assert_eq!(pid_slot(1), 277);
        assert_eq!(pid_slot(2), 42);
        assert_eq!(pid_slot(512 + 1), 512 + 277);
        assert_eq!(
            pid_slot(u32::MAX as usize),
            (u32::MAX as usize & !511) | ((511 * 277) & 511)
        );
    }
}

#[cfg(test)]
mod fentry_tests {
    use super::{can_fentry, release_version};

    #[test]
    fn release_versions_parse_as_numbers() {
        assert_eq!(release_version("6.10.0"), Some((6, 10)));
        assert_eq!(release_version("6.4-rc1"), Some((6, 4)));
        assert_eq!(release_version("6.12.75+deb13-arm64\n"), Some((6, 12)));
        assert_eq!(release_version("7.0.0-rc1"), Some((7, 0)));
        assert_eq!(release_version(""), None);
    }

    #[test]
    fn x86_64_allows_six_arguments_before_6_6() {
        assert!(can_fentry(
            "x86_64",
            "5.10.240-238.959.amzn2.x86_64",
            None,
            6
        ));
        assert!(!can_fentry(
            "x86_64",
            "5.10.240-238.959.amzn2.x86_64",
            None,
            7
        ));
        assert!(!can_fentry("x86_64", "6.5.0", None, 7));
        assert!(can_fentry("x86_64", "6.6.0", None, 7));
        assert!(can_fentry("x86_64", "6.12.63+deb13-amd64", None, 7));
        assert!(!can_fentry("x86_64", "6.12.63+deb13-amd64", None, 13));
        assert!(!can_fentry("x86_64", "", None, 7));
        assert!(can_fentry("x86_64", "", None, 6));
    }

    #[test]
    fn aarch64_needs_direct_calls() {
        assert!(!can_fentry(
            "aarch64",
            "5.10.240-238.959.amzn2.aarch64",
            None,
            2
        ));
        assert!(!can_fentry("aarch64", "6.1.0-40-arm64", None, 2));
        assert!(!can_fentry("aarch64", "6.3.13", None, 2));
        assert!(can_fentry("aarch64", "6.4-rc1", None, 2));
        assert!(can_fentry("aarch64", "6.10.0", None, 7));
        assert!(!can_fentry("aarch64", "6.10.0", None, 9));
        assert!(!can_fentry("aarch64", "", None, 2));
    }

    #[test]
    fn aarch64_kernel_config_decides_when_present() {
        let with = "CONFIG_FTRACE=y\nCONFIG_DYNAMIC_FTRACE_WITH_DIRECT_CALLS=y\n";
        let without = "CONFIG_FTRACE=y\n# CONFIG_DYNAMIC_FTRACE_WITH_DIRECT_CALLS is not set\n";
        assert!(can_fentry("aarch64", "6.1.0", Some(with), 2));
        assert!(!can_fentry("aarch64", "6.12.0", Some(without), 2));
        assert!(can_fentry("x86_64", "6.12.0", Some(without), 2));
    }
}
