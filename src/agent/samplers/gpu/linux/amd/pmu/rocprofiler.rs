//! Minimal safe wrapper around rocprofiler-sdk + the HSA runtime for reading
//! **device-wide** GPU hardware performance counters.
//!
//! As with [`super::super::rocm_smi`], the libraries are `dlopen`d at runtime
//! via `libloading` rather than linked at build time, so the agent compiles on
//! hosts without ROCm. We declare only the handful of C functions we need.
//! Signatures come from the rocprofiler-sdk headers (ROCm 7.x).
//!
//! ## How device counting works
//!
//! rocprofiler-sdk has no device-wide CLI; the supported path is the
//! **device counting service** C API. It reads counters for the whole GPU over
//! a wall-clock window, aggregating activity from *all* processes — exactly
//! what an always-on agent needs. The agent itself runs no GPU kernels.
//!
//! ## Activation and the "configuration locked" constraint
//!
//! rocprofiler can only create contexts/buffers and configure services from
//! **inside the tool `initialize` callback** that it invokes during runtime
//! startup. Calling `rocprofiler_create_context` afterwards returns
//! `CONFIGURATION_LOCKED`. So the flow is:
//!
//! 1. `rocprofiler_force_configure(configure)` — register our configure
//!    callback. Must happen before any ROCm runtime initializes.
//! 2. `hsa_init()` — brings up ROCr and calls our `configure`, which returns a
//!    result whose `initialize` pointer rocprofiler then invokes. **All context
//!    / buffer / device-counting setup happens in that `initialize`** (see
//!    [`tool_initialize`]).
//! 3. Each sample: `start_context` → wait window →
//!    `sample_device_counting_service` → `stop_context`, then sum the
//!    per-hardware-instance records into one device-level value per counter.
//!
//! Because rocprofiler is inherently a single per-process tool, the loaded
//! library handle and the per-agent state live in a process-global
//! ([`STATE`]). [`Rocprofiler`] is a thin handle that the sampler owns; its
//! `Drop` shuts down HSA.
//!
//! Requires the `CAP_PERFMON` capability. On RDNA GPUs some per-SIMD counters
//! only read non-zero when the GPU is in a stable power state; we do not change
//! the power state here (see `pmu/mod.rs`).

#![allow(non_camel_case_types)]

use libloading::{Library, Symbol};
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// C types (all rocprofiler handles are a single u64 `handle`).
// ---------------------------------------------------------------------------

type RocprofilerStatus = u32;
const ROCPROFILER_STATUS_SUCCESS: RocprofilerStatus = 0;

type HsaStatus = u32;
const HSA_STATUS_SUCCESS: HsaStatus = 0;

// Enum values from rocprofiler-sdk headers. These enums begin with a
// `*_NONE = 0` variant, so the first real ("version 0") values are 1.
const ROCPROFILER_AGENT_INFO_VERSION_0: u32 = 1;
const ROCPROFILER_COUNTER_INFO_VERSION_0: u32 = 1;
const ROCPROFILER_AGENT_TYPE_GPU: u32 = 2; // NONE=0, CPU=1, GPU=2
const ROCPROFILER_BUFFER_POLICY_LOSSLESS: u32 = 2; // NONE=0, DISCARD=1, LOSSLESS=2
const ROCPROFILER_COUNTER_FLAG_NONE: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct ContextId {
    handle: u64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct BufferId {
    handle: u64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct AgentId {
    handle: u64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CounterId {
    handle: u64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CounterConfigId {
    handle: u64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CallbackThreadId {
    handle: u64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct UserData {
    value: u64,
}

/// `rocprofiler_counter_record_t` — must match the C layout exactly: `id`
/// (u64), `counter_value` (f64), `dispatch_id` (u64), `user_data` (u64),
/// `agent_id` (u64) = 40 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
struct CounterRecord {
    id: u64,
    counter_value: f64,
    dispatch_id: u64,
    user_data: UserData,
    agent_id: AgentId,
}

/// `rocprofiler_counter_info_v0_t`. We only read `id` and `name`.
#[repr(C)]
struct CounterInfoV0 {
    id: CounterId,
    name: *const c_char,
    description: *const c_char,
    block: *const c_char,
    expression: *const c_char,
    flags: u8,
}

// Callback function pointer types.
type QueryAgentsCb = unsafe extern "C" fn(
    version: u32,
    agents: *const *const c_void,
    num: usize,
    udata: *mut c_void,
) -> RocprofilerStatus;
type IterateCountersCb = unsafe extern "C" fn(
    agent: AgentId,
    counters: *const CounterId,
    num: usize,
    udata: *mut c_void,
) -> RocprofilerStatus;
type BufferCb =
    unsafe extern "C" fn(ContextId, BufferId, *mut *mut c_void, usize, *mut c_void, u64);
/// `rocprofiler_device_counting_agent_cb_t`.
type SetConfigCb = unsafe extern "C" fn(ContextId, CounterConfigId) -> RocprofilerStatus;
/// `rocprofiler_device_counting_service_cb_t`.
type DeviceCountingCb = unsafe extern "C" fn(ContextId, AgentId, SetConfigCb, *mut c_void);
/// `rocprofiler_configure` function pointer passed to `force_configure`.
type ConfigureFunc = unsafe extern "C" fn(
    version: u32,
    runtime_version: *const c_char,
    priority: u32,
    client_id: *mut c_void,
) -> *mut c_void;
/// `rocprofiler_tool_initialize_t`: `int (*)(finalize_func, tool_data)`.
type ToolInitialize = unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32;

// rocprofiler-sdk function signatures.
type FnForceConfigure = unsafe extern "C" fn(ConfigureFunc) -> RocprofilerStatus;
type FnQueryAgents =
    unsafe extern "C" fn(u32, QueryAgentsCb, usize, *mut c_void) -> RocprofilerStatus;
type FnCreateContext = unsafe extern "C" fn(*mut ContextId) -> RocprofilerStatus;
type FnStartContext = unsafe extern "C" fn(ContextId) -> RocprofilerStatus;
type FnStopContext = unsafe extern "C" fn(ContextId) -> RocprofilerStatus;
type FnCreateBuffer = unsafe extern "C" fn(
    ContextId,
    usize,
    usize,
    u32,
    BufferCb,
    *mut c_void,
    *mut BufferId,
) -> RocprofilerStatus;
type FnCreateCbThread = unsafe extern "C" fn(*mut CallbackThreadId) -> RocprofilerStatus;
type FnAssignCbThread = unsafe extern "C" fn(BufferId, CallbackThreadId) -> RocprofilerStatus;
type FnConfigureDeviceCounting =
    unsafe extern "C" fn(ContextId, BufferId, AgentId, DeviceCountingCb, *mut c_void)
        -> RocprofilerStatus;
type FnSampleDeviceCounting =
    unsafe extern "C" fn(ContextId, UserData, u32, *mut CounterRecord, *mut usize)
        -> RocprofilerStatus;
type FnIterateCounters =
    unsafe extern "C" fn(AgentId, IterateCountersCb, *mut c_void) -> RocprofilerStatus;
type FnQueryCounterInfo = unsafe extern "C" fn(CounterId, u32, *mut c_void) -> RocprofilerStatus;
type FnQueryRecordCounterId = unsafe extern "C" fn(u64, *mut CounterId) -> RocprofilerStatus;
type FnCreateCounterConfig =
    unsafe extern "C" fn(AgentId, *const CounterId, usize, *mut CounterConfigId)
        -> RocprofilerStatus;

type FnHsaInit = unsafe extern "C" fn() -> HsaStatus;
type FnHsaShutDown = unsafe extern "C" fn() -> HsaStatus;

// ---------------------------------------------------------------------------
// Loaded symbols + per-agent state, owned by a process-global.
//
// rocprofiler is inherently a single per-process tool, and setup must happen
// inside the `initialize` callback (see module docs). We therefore keep the
// resolved symbols and per-agent contexts in a global the callback populates.
// ---------------------------------------------------------------------------

/// The resolved rocprofiler-sdk symbols (all but force_configure, which is only
/// needed once during construction).
struct Syms {
    query_agents: Symbol<'static, FnQueryAgents>,
    create_context: Symbol<'static, FnCreateContext>,
    start_context: Symbol<'static, FnStartContext>,
    stop_context: Symbol<'static, FnStopContext>,
    create_buffer: Symbol<'static, FnCreateBuffer>,
    create_cb_thread: Symbol<'static, FnCreateCbThread>,
    assign_cb_thread: Symbol<'static, FnAssignCbThread>,
    configure_device_counting: Symbol<'static, FnConfigureDeviceCounting>,
    sample_device_counting: Symbol<'static, FnSampleDeviceCounting>,
    iterate_counters: Symbol<'static, FnIterateCounters>,
    query_counter_info: Symbol<'static, FnQueryCounterInfo>,
    query_record_counter_id: Symbol<'static, FnQueryRecordCounterId>,
    create_counter_config: Symbol<'static, FnCreateCounterConfig>,
}
// SAFETY: the symbols are plain C function pointers; calling them is guarded by
// the STATE mutex. They borrow from libraries kept alive for the process.
unsafe impl Send for Syms {}

/// Per-GPU-agent device-counting state. The context and counter config are
/// built in the `tool_initialize` callback, then the context is started once
/// after `hsa_init()` (it cannot be started before HSA is loaded) and runs
/// until shutdown.
struct AgentState {
    ctx: ContextId,
    /// The single-pass counter config; passed to the device-counting callback
    /// via the `PENDING_CONFIG` thread-local when the context is started.
    config: CounterConfigId,
    /// Counter id -> name, for attributing records.
    id_to_name: HashMap<u64, String>,
    /// The counter names successfully included in the config (a subset of the
    /// requested set), retained for diagnostics/logging.
    kept: Vec<String>,
    /// Whether `start_context` succeeded for this agent.
    started: bool,
}
// SAFETY: only accessed under the STATE mutex.
unsafe impl Send for AgentState {}

struct State {
    syms: Option<Syms>,
    agents: Vec<AgentState>,
    /// The counter names the initialize callback should program. Set before
    /// `hsa_init()`.
    wanted: Vec<String>,
    /// Result of the initialize callback, surfaced to `Rocprofiler::new`.
    init_error: Option<String>,
    initialized: bool,
    hsa_shut_down: Option<Symbol<'static, FnHsaShutDown>>,
    /// Keep the dlopen handles alive for the whole process.
    _libs: Vec<Box<Library>>,
}

// SAFETY: all access is serialized through the Mutex; the contained symbols are
// C function pointers / handles valid for the process lifetime.
unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);

// The device-counting "set config" callback reads the config handle for the
// agent being configured from a thread-local set just before `start_context`.
// rocprofiler invokes the service callback synchronously from within
// `start_context` on the same thread.
thread_local! {
    static PENDING_CONFIG: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

// ---------------------------------------------------------------------------
// configure / initialize callbacks (called by rocprofiler during hsa_init).
// ---------------------------------------------------------------------------

#[repr(C)]
struct ToolConfigureResult {
    size: usize,
    initialize: Option<ToolInitialize>,
    finalize: *const c_void,
    tool_data: *mut c_void,
}
unsafe impl Sync for ToolConfigureResult {}

static CONFIGURE_RESULT: ToolConfigureResult = ToolConfigureResult {
    size: std::mem::size_of::<ToolConfigureResult>(),
    initialize: Some(tool_initialize),
    finalize: std::ptr::null(),
    tool_data: std::ptr::null_mut(),
};

unsafe extern "C" fn rocp_configure(
    _version: u32,
    _runtime_version: *const c_char,
    _priority: u32,
    _client_id: *mut c_void,
) -> *mut c_void {
    &CONFIGURE_RESULT as *const ToolConfigureResult as *mut c_void
}

/// Invoked by rocprofiler during runtime init — the only place contexts may be
/// created. Enumerates GPU agents and builds a device-counting context +
/// single-pass counter config for each, storing them in [`STATE`].
unsafe extern "C" fn tool_initialize(_finalize: *mut c_void, _tool_data: *mut c_void) -> i32 {
    let mut guard = match STATE.lock() {
        Ok(g) => g,
        Err(_) => return -1,
    };
    let state = match guard.as_mut() {
        Some(s) => s,
        None => return -1,
    };

    if let Err(e) = setup_all_agents(state) {
        state.init_error = Some(e);
    }
    state.initialized = true;
    0
}

fn setup_all_agents(state: &mut State) -> Result<(), String> {
    let syms = state.syms.as_ref().ok_or("symbols not loaded")?;

    // Enumerate GPU agents.
    let mut ids: Vec<AgentId> = Vec::new();
    unsafe {
        let st = (syms.query_agents)(
            ROCPROFILER_AGENT_INFO_VERSION_0,
            query_agents_cb,
            ROCPROFILER_AGENT_STRUCT_SIZE,
            &mut ids as *mut _ as *mut c_void,
        );
        if st != ROCPROFILER_STATUS_SUCCESS {
            return Err(format!("query_available_agents failed: {st}"));
        }
    }

    let wanted: Vec<&str> = state.wanted.iter().map(|s| s.as_str()).collect();
    let mut built = Vec::new();
    for (idx, id) in ids.into_iter().enumerate() {
        match setup_agent(syms, id, &wanted) {
            Ok(a) => built.push(a),
            Err(e) => crate::debug!("gpu_amd_pmu: GPU {idx} skipped: {e}"),
        }
    }
    if built.is_empty() {
        return Err("no GPU agents support the requested counters".into());
    }
    state.agents = built;
    Ok(())
}

fn setup_agent(syms: &Syms, id: AgentId, wanted: &[&str]) -> Result<AgentState, String> {
    unsafe {
        let mut ctx = ContextId { handle: 0 };
        let st = (syms.create_context)(&mut ctx);
        if st != ROCPROFILER_STATUS_SUCCESS {
            return Err(format!("create_context failed: {st}"));
        }

        let mut buf = BufferId { handle: 0 };
        if (syms.create_buffer)(
            ctx,
            BUFFER_BYTES,
            BUFFER_WATERMARK,
            ROCPROFILER_BUFFER_POLICY_LOSSLESS,
            noop_buffer_cb,
            std::ptr::null_mut(),
            &mut buf,
        ) != ROCPROFILER_STATUS_SUCCESS
        {
            return Err("create_buffer failed".into());
        }

        let mut th = CallbackThreadId { handle: 0 };
        if (syms.create_cb_thread)(&mut th) != ROCPROFILER_STATUS_SUCCESS {
            return Err("create_callback_thread failed".into());
        }
        if (syms.assign_cb_thread)(buf, th) != ROCPROFILER_STATUS_SUCCESS {
            return Err("assign_callback_thread failed".into());
        }

        if (syms.configure_device_counting)(ctx, buf, id, device_counting_cb, std::ptr::null_mut())
            != ROCPROFILER_STATUS_SUCCESS
        {
            return Err("configure_device_counting_service failed".into());
        }

        // Resolve counter names -> ids for this agent.
        let supported = supported_counters(syms, id)?;
        let mut counter_ids = Vec::new();
        let mut id_to_name = HashMap::new();
        let mut kept = Vec::new();
        for &name in wanted {
            if let Some(&cid) = supported.get(name) {
                counter_ids.push(cid);
                id_to_name.insert(cid.handle, name.to_string());
                kept.push(name.to_string());
            }
        }
        if counter_ids.is_empty() {
            return Err("none of the requested counters are supported".into());
        }

        let mut cfg = CounterConfigId { handle: 0 };
        let st =
            (syms.create_counter_config)(id, counter_ids.as_ptr(), counter_ids.len(), &mut cfg);
        if st != ROCPROFILER_STATUS_SUCCESS {
            return Err(format!(
                "create_counter_config failed ({st}); the set may exceed the \
                 single-pass per-block slot budget"
            ));
        }

        // NOTE: the context is NOT started here. `tool_initialize` runs inside
        // `rocprofiler_force_configure`, which executes BEFORE `hsa_init()`, and
        // `start_context` requires HSA to be loaded (else HSA_NOT_LOADED). The
        // contexts are started later in `Rocprofiler::new`, after `hsa_init()`.
        Ok(AgentState {
            ctx,
            config: cfg,
            id_to_name,
            kept,
            started: false,
        })
    }
}

fn supported_counters(syms: &Syms, id: AgentId) -> Result<HashMap<String, CounterId>, String> {
    let mut ids: Vec<CounterId> = Vec::new();
    unsafe {
        if (syms.iterate_counters)(id, iterate_counters_cb, &mut ids as *mut _ as *mut c_void)
            != ROCPROFILER_STATUS_SUCCESS
        {
            return Err("iterate_agent_supported_counters failed".into());
        }
    }
    let mut map = HashMap::with_capacity(ids.len());
    for cid in ids {
        if let Some(name) = counter_name(syms, cid) {
            map.insert(name, cid);
        }
    }
    Ok(map)
}

fn counter_name(syms: &Syms, cid: CounterId) -> Option<String> {
    unsafe {
        let mut info: CounterInfoV0 = std::mem::zeroed();
        if (syms.query_counter_info)(
            cid,
            ROCPROFILER_COUNTER_INFO_VERSION_0,
            &mut info as *mut _ as *mut c_void,
        ) != ROCPROFILER_STATUS_SUCCESS
            || info.name.is_null()
        {
            return None;
        }
        Some(CStr::from_ptr(info.name).to_string_lossy().into_owned())
    }
}

unsafe extern "C" fn device_counting_cb(
    ctx: ContextId,
    _agent: AgentId,
    set_config: SetConfigCb,
    _udata: *mut c_void,
) {
    let handle = PENDING_CONFIG.with(|c| c.get());
    if handle != 0 {
        let _ = set_config(ctx, CounterConfigId { handle });
    }
}

// ---------------------------------------------------------------------------
// Public handle.
// ---------------------------------------------------------------------------

/// Handle to the process-global rocprofiler state. Constructing it loads the
/// libraries, force-configures, and runs `hsa_init()` (which performs setup).
/// Dropping it shuts down HSA. Only one may exist at a time.
pub struct Rocprofiler {
    /// Number of GPU agents that were set up successfully.
    num_agents: usize,
}

impl Rocprofiler {
    /// Load the libraries, register, and initialize for the given counter set.
    /// Returns `Ok(None)` if rocprofiler-sdk / HSA are not present.
    pub fn new(wanted: &[&str]) -> Result<Option<Self>, String> {
        let mut guard = STATE.lock().map_err(|_| "rocprofiler state poisoned")?;
        if guard.is_some() {
            return Err("rocprofiler already initialized in this process".into());
        }

        // SAFETY: loading system shared libraries is inherently unsafe; we
        // trust the ROCm-provided libraries.
        let rocp = match unsafe {
            load_first(&["librocprofiler-sdk.so", "librocprofiler-sdk.so.1"])
        } {
            Some(lib) => Box::new(lib),
            None => return Ok(None),
        };
        let hsa = match unsafe { load_first(&["libhsa-runtime64.so", "libhsa-runtime64.so.1"]) } {
            Some(lib) => Box::new(lib),
            None => return Ok(None),
        };

        let (syms, force_configure, hsa_init, hsa_shut_down) = unsafe {
            let syms = Syms {
                query_agents: required(&rocp, b"rocprofiler_query_available_agents")?,
                create_context: required(&rocp, b"rocprofiler_create_context")?,
                start_context: required(&rocp, b"rocprofiler_start_context")?,
                stop_context: required(&rocp, b"rocprofiler_stop_context")?,
                create_buffer: required(&rocp, b"rocprofiler_create_buffer")?,
                create_cb_thread: required(&rocp, b"rocprofiler_create_callback_thread")?,
                assign_cb_thread: required(&rocp, b"rocprofiler_assign_callback_thread")?,
                configure_device_counting: required(
                    &rocp,
                    b"rocprofiler_configure_device_counting_service",
                )?,
                sample_device_counting: required(
                    &rocp,
                    b"rocprofiler_sample_device_counting_service",
                )?,
                iterate_counters: required(
                    &rocp,
                    b"rocprofiler_iterate_agent_supported_counters",
                )?,
                query_counter_info: required(&rocp, b"rocprofiler_query_counter_info")?,
                query_record_counter_id: required(
                    &rocp,
                    b"rocprofiler_query_record_counter_id",
                )?,
                create_counter_config: required(&rocp, b"rocprofiler_create_counter_config")?,
            };
            let force_configure: Symbol<FnForceConfigure> =
                required(&rocp, b"rocprofiler_force_configure")?;
            let hsa_init: Symbol<FnHsaInit> = required(&hsa, b"hsa_init")?;
            let hsa_shut_down: Symbol<FnHsaShutDown> = required(&hsa, b"hsa_shut_down")?;
            (syms, force_configure, hsa_init, hsa_shut_down)
        };

        *guard = Some(State {
            syms: Some(syms),
            agents: Vec::new(),
            wanted: wanted.iter().map(|s| s.to_string()).collect(),
            init_error: None,
            initialized: false,
            hsa_shut_down: Some(hsa_shut_down),
            _libs: vec![rocp, hsa],
        });

        // `force_configure` invokes our `tool_initialize` **synchronously and
        // immediately** (not during hsa_init as one might expect), and
        // `tool_initialize` re-locks STATE. So we must release the guard before
        // calling it to avoid self-deadlock.
        drop(guard);

        let st = unsafe { force_configure(rocp_configure) };
        if st != ROCPROFILER_STATUS_SUCCESS {
            *STATE.lock().map_err(|_| "rocprofiler state poisoned")? = None;
            return Err(format!("rocprofiler_force_configure failed: {st}"));
        }

        // tool_initialize has now run and built the per-agent contexts and
        // counter configs (but did NOT start them — HSA wasn't up yet). Bring up
        // HSA so the device counting service is live.
        let hsa_st = unsafe { hsa_init() };

        let mut guard = STATE.lock().map_err(|_| "rocprofiler state poisoned")?;
        let state = guard.as_mut().ok_or("rocprofiler state vanished")?;

        if hsa_st != HSA_STATUS_SUCCESS {
            *guard = None;
            return Err(format!("hsa_init failed: {hsa_st}"));
        }
        if !state.initialized {
            *guard = None;
            return Err("rocprofiler tool_initialize was not invoked".into());
        }
        if let Some(e) = state.init_error.take() {
            *guard = None;
            return Err(e);
        }

        // Now that HSA is loaded, start each agent's context once. It runs
        // continuously, so the counters accumulate and each later read returns
        // the running cumulative total. The device-counting callback fires
        // synchronously inside start_context on this thread and reads the config
        // from the PENDING_CONFIG thread-local, so set it just before.
        if let Some(syms) = state.syms.as_ref() {
            for agent in state.agents.iter_mut() {
                PENDING_CONFIG.with(|c| c.set(agent.config.handle));
                let st = unsafe { (syms.start_context)(agent.ctx) };
                PENDING_CONFIG.with(|c| c.set(0));
                agent.started = st == ROCPROFILER_STATUS_SUCCESS;
                if !agent.started {
                    crate::debug!("gpu_amd_pmu: start_context failed: {st}");
                }
            }
        }
        state.agents.retain(|a| a.started);

        let num_agents = state.agents.len();
        if num_agents == 0 {
            *guard = None;
            return Err("no GPU agent contexts could be started".into());
        }

        // Log which counters each agent ended up with.
        for (idx, a) in state.agents.iter().enumerate() {
            if a.kept.len() != wanted.len() {
                crate::debug!(
                    "gpu_amd_pmu: GPU {idx}: collecting {}/{} requested counters",
                    a.kept.len(),
                    wanted.len()
                );
            }
        }

        Ok(Some(Self { num_agents }))
    }

    /// Number of GPU agents set up.
    pub fn num_agents(&self) -> usize {
        self.num_agents
    }

    /// Read the current cumulative counter totals for agent `idx`. Returns a
    /// map of counter name -> device-level cumulative value (summed across
    /// hardware instances).
    ///
    /// The context was started once at init and runs continuously, so each read
    /// returns the running total since startup — a monotonic counter, ideal for
    /// `rate()` downstream. The `STATE` mutex serializes reads: rocprofiler
    /// rejects overlapping `sample_device_counting_service` calls with
    /// `CONTEXT_ERROR`, and the agent's async `refresh()` can run concurrently.
    pub fn sample(&self, idx: usize) -> Result<HashMap<String, f64>, String> {
        let guard = STATE.lock().map_err(|_| "rocprofiler state poisoned")?;
        let state = guard.as_ref().ok_or("rocprofiler not initialized")?;
        let syms = state.syms.as_ref().ok_or("symbols missing")?;
        let agent = state.agents.get(idx).ok_or("agent index out of range")?;

        let ctx = agent.ctx;

        let mut out = vec![
            CounterRecord {
                id: 0,
                counter_value: 0.0,
                dispatch_id: 0,
                user_data: UserData { value: 0 },
                agent_id: AgentId { handle: 0 },
            };
            MAX_RECORDS
        ];
        let mut count = out.len();

        let st = unsafe {
            (syms.sample_device_counting)(
                ctx,
                UserData { value: 0 },
                ROCPROFILER_COUNTER_FLAG_NONE,
                out.as_mut_ptr(),
                &mut count,
            )
        };

        if st != ROCPROFILER_STATUS_SUCCESS {
            return Err(format!("sample_device_counting_service failed: {st}"));
        }
        out.truncate(count);

        let mut sums: HashMap<String, f64> = HashMap::new();
        for rec in &out {
            let mut cid = CounterId { handle: 0 };
            unsafe {
                if (syms.query_record_counter_id)(rec.id, &mut cid) != ROCPROFILER_STATUS_SUCCESS {
                    continue;
                }
            }
            if let Some(name) = agent.id_to_name.get(&cid.handle) {
                *sums.entry(name.clone()).or_insert(0.0) += rec.counter_value;
            }
        }
        Ok(sums)
    }
}

impl Drop for Rocprofiler {
    fn drop(&mut self) {
        if let Ok(mut guard) = STATE.lock() {
            if let Some(state) = guard.as_mut() {
                // Stop the per-agent contexts we started at init.
                if let Some(syms) = state.syms.as_ref() {
                    for agent in state.agents.iter().filter(|a| a.started) {
                        // SAFETY: balances the start_context in `new`.
                        unsafe {
                            let _ = (syms.stop_context)(agent.ctx);
                        }
                    }
                }
                if let Some(shut_down) = state.hsa_shut_down.take() {
                    // SAFETY: balances the hsa_init() in `new`.
                    unsafe {
                        let _ = shut_down();
                    }
                }
            }
            *guard = None;
        }
    }
}

// ---------------------------------------------------------------------------
// callbacks / helpers
// ---------------------------------------------------------------------------

/// `sizeof(rocprofiler_agent_t)` passed to `query_available_agents`, validated
/// against the library's own struct size (mismatch -> INCOMPATIBLE_ABI).
/// Measured as 312 on ROCm 7.2.1; we only read the v0 prefix so the trailing
/// layout is irrelevant.
const ROCPROFILER_AGENT_STRUCT_SIZE: usize = 312;

/// `rocprofiler_agent_v0_t` prefix: `size` (u64), `id` (AgentId), `type` (u32).
#[repr(C)]
struct AgentV0Prefix {
    size: u64,
    id: AgentId,
    ty: u32,
}

unsafe extern "C" fn query_agents_cb(
    version: u32,
    agents: *const *const c_void,
    num: usize,
    udata: *mut c_void,
) -> RocprofilerStatus {
    if version != ROCPROFILER_AGENT_INFO_VERSION_0 {
        return ROCPROFILER_STATUS_SUCCESS;
    }
    let out = &mut *(udata as *mut Vec<AgentId>);
    for i in 0..num {
        let p = *agents.add(i) as *const AgentV0Prefix;
        if p.is_null() {
            continue;
        }
        let a = &*p;
        if a.ty == ROCPROFILER_AGENT_TYPE_GPU {
            out.push(a.id);
        }
    }
    ROCPROFILER_STATUS_SUCCESS
}

unsafe extern "C" fn iterate_counters_cb(
    _agent: AgentId,
    counters: *const CounterId,
    num: usize,
    udata: *mut c_void,
) -> RocprofilerStatus {
    let out = &mut *(udata as *mut Vec<CounterId>);
    for i in 0..num {
        out.push(*counters.add(i));
    }
    ROCPROFILER_STATUS_SUCCESS
}

unsafe extern "C" fn noop_buffer_cb(
    _ctx: ContextId,
    _buf: BufferId,
    _headers: *mut *mut c_void,
    _num: usize,
    _udata: *mut c_void,
    _drop: u64,
) {
}

const BUFFER_BYTES: usize = 16384;
const BUFFER_WATERMARK: usize = 8192;
/// Counters return one record per hardware instance (SE × SA × WGP …). 8192 is
/// comfortably above the largest instance count seen (per-WGP ≈ 32–64).
const MAX_RECORDS: usize = 8192;

unsafe fn required<T>(lib: &Library, name: &[u8]) -> Result<Symbol<'static, T>, String> {
    let sym: Symbol<T> = lib
        .get(name)
        .map_err(|e| format!("missing symbol {}: {e}", String::from_utf8_lossy(name)))?;
    Ok(std::mem::transmute::<Symbol<T>, Symbol<'static, T>>(sym))
}

unsafe fn load_first(names: &[&str]) -> Option<Library> {
    for n in names {
        if let Ok(lib) = Library::new(n) {
            return Some(lib);
        }
    }
    None
}
