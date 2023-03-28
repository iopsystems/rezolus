/* SPDX-License-Identifier: (MIT OR Apache-2.0) */
/* Copyright (c) 2022 Jim Blandy */

//! Events we can monitor or count.
//!
//! There are three general categories of event:
//!
//! -   [`Hardware`] events are counted by the processor itself. This
//!     includes things like clock cycles, instructions retired, and cache and
//!     branch prediction statistics.
//!
//! -   [`Cache`] events, also counted by the processor, offer a more
//!     detailed view of the processor's cache counters. You can
//!     select which level of the cache hierarchy to observe,
//!     discriminate between data and instruction caches, and so on.
//!
//! -   [`Software`] events are counted by the kernel. This includes things
//!     like context switches, page faults, and so on.
//!
//! -   [`Breakpoint`] events correspond to hardware breakpoints. They can
//!     count read/write accesses to an address as well as execution of an
//!     instruction address.
//!
//! The `Event` type is just an enum with a variant for each of the above types,
//! which all implement `Into<Event>`.
//!
//! Linux supports many more kinds of events than this module covers, including
//! events specific to particular make and model of processor, and events that
//! are dynamically registered by drivers and kernel modules. If something you
//! want is missing, think about the best API to expose it, and submit a pull
//! request!
//!
//! [`Hardware`]: enum.Hardware.html
//! [`Software`]: enum.Software.html
//! [`Cache`]: struct.Cache.html

#![allow(non_camel_case_types)]
use bitflags::bitflags;
use perf_event_open_sys::bindings;

/// Any sort of event. This is a sum of the [`Hardware`],
/// [`Software`], and [`Cache`] types, which all implement
/// `Into<Event>`.
///
/// [`Hardware`]: enum.Hardware.html
/// [`Software`]: enum.Software.html
/// [`Cache`]: struct.Cache.html
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    #[allow(missing_docs)]
    Hardware(Hardware),

    #[allow(missing_docs)]
    Software(Software),

    #[allow(missing_docs)]
    Cache(Cache),

    #[allow(missing_docs)]
    Breakpoint(Breakpoint),
}

impl Event {
    pub(crate) fn update_attrs(self, attr: &mut bindings::perf_event_attr) {
        match self {
            Event::Hardware(hw) => {
                attr.type_ = bindings::PERF_TYPE_HARDWARE;
                attr.config = hw as _;
            }
            Event::Software(sw) => {
                attr.type_ = bindings::PERF_TYPE_SOFTWARE;
                attr.config = sw as _;
            }
            Event::Cache(cache) => {
                attr.type_ = bindings::PERF_TYPE_HW_CACHE;
                attr.config = cache.as_config();
            }
            Event::Breakpoint(bp) => {
                attr.type_ = bindings::PERF_TYPE_BREAKPOINT;
                // Clear config in case it was set by a previous call to update_attrs
                attr.config = 0;

                match bp {
                    Breakpoint::Data { access, addr, len } => {
                        attr.bp_type = access.bits();
                        attr.__bindgen_anon_3.bp_addr = addr;
                        attr.__bindgen_anon_4.bp_len = len;
                    }
                    Breakpoint::Code { addr } => {
                        attr.bp_type = bindings::HW_BREAKPOINT_X;
                        attr.__bindgen_anon_3.bp_addr = addr;
                        // According to the perf_event_open man page, execute breakpoints
                        // should set len to sizeof(long).
                        attr.__bindgen_anon_4.bp_len = std::mem::size_of::<libc::c_long>() as _;
                    }
                }
            }
        }
    }
}

/// Hardware counters.
///
/// These are counters implemented by the processor itself. Such counters vary
/// from one architecture to the next, and even different models within a
/// particular architecture will often change the way they expose this data.
/// This is a selection of portable names for values that can be obtained on a
/// wide variety of systems.
///
/// Each variant of this enum corresponds to a particular `PERF_COUNT_HW_`...
/// value supported by the [`perf_event_open`][man] system call.
///
/// [man]: http://man7.org/linux/man-pages/man2/perf_event_open.2.html
#[repr(u32)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Hardware {
    /// Total cycles.
    CPU_CYCLES = bindings::PERF_COUNT_HW_CPU_CYCLES,

    /// Retired instructions.
    INSTRUCTIONS = bindings::PERF_COUNT_HW_INSTRUCTIONS,

    /// Cache accesses.
    CACHE_REFERENCES = bindings::PERF_COUNT_HW_CACHE_REFERENCES,

    /// Cache misses.
    CACHE_MISSES = bindings::PERF_COUNT_HW_CACHE_MISSES,

    /// Retired branch instructions.
    BRANCH_INSTRUCTIONS = bindings::PERF_COUNT_HW_BRANCH_INSTRUCTIONS,

    /// Mispredicted branch instructions.
    BRANCH_MISSES = bindings::PERF_COUNT_HW_BRANCH_MISSES,

    /// Bus cycles.
    BUS_CYCLES = bindings::PERF_COUNT_HW_BUS_CYCLES,

    /// Stalled cycles during issue.
    STALLED_CYCLES_FRONTEND = bindings::PERF_COUNT_HW_STALLED_CYCLES_FRONTEND,

    /// Stalled cycles during retirement.
    STALLED_CYCLES_BACKEND = bindings::PERF_COUNT_HW_STALLED_CYCLES_BACKEND,

    /// Total cycles, independent of frequency scaling.
    REF_CPU_CYCLES = bindings::PERF_COUNT_HW_REF_CPU_CYCLES,
}

impl From<Hardware> for Event {
    fn from(hw: Hardware) -> Event {
        Event::Hardware(hw)
    }
}

/// Software counters, implemented by the kernel.
///
/// Each variant of this enum corresponds to a particular `PERF_COUNT_SW_`...
/// value supported by the [`perf_event_open`][man] system call.
///
/// [man]: http://man7.org/linux/man-pages/man2/perf_event_open.2.html
#[repr(u32)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Software {
    /// High-resolution per-CPU timer.
    CPU_CLOCK = bindings::PERF_COUNT_SW_CPU_CLOCK,

    /// Per-task clock count.
    TASK_CLOCK = bindings::PERF_COUNT_SW_TASK_CLOCK,

    /// Page faults.
    PAGE_FAULTS = bindings::PERF_COUNT_SW_PAGE_FAULTS,

    /// Context switches.
    CONTEXT_SWITCHES = bindings::PERF_COUNT_SW_CONTEXT_SWITCHES,

    /// Process migration to another CPU.
    CPU_MIGRATIONS = bindings::PERF_COUNT_SW_CPU_MIGRATIONS,

    /// Minor page faults: resolved without needing I/O.
    PAGE_FAULTS_MIN = bindings::PERF_COUNT_SW_PAGE_FAULTS_MIN,

    /// Major page faults: I/O was required to resolve these.
    PAGE_FAULTS_MAJ = bindings::PERF_COUNT_SW_PAGE_FAULTS_MAJ,

    /// Alignment faults that required kernel intervention.
    ///
    /// This is only generated on some CPUs, and never on x86_64 or
    /// ARM.
    ALIGNMENT_FAULTS = bindings::PERF_COUNT_SW_ALIGNMENT_FAULTS,

    /// Instruction emulation faults.
    EMULATION_FAULTS = bindings::PERF_COUNT_SW_EMULATION_FAULTS,

    /// Placeholder, for collecting informational sample records.
    DUMMY = bindings::PERF_COUNT_SW_DUMMY,
}

impl From<Software> for Event {
    fn from(hw: Software) -> Event {
        Event::Software(hw)
    }
}

/// A cache event.
///
/// A cache event has three identifying characteristics:
///
/// - which cache to observe ([`which`])
///
/// - what sort of request it's handling ([`operation`])
///
/// - whether we want to count all cache accesses, or just misses
///   ([`result`]).
///
/// For example, to measure the L1 data cache's miss rate:
///
///     # use perf_event::{Builder, Group};
///     # use perf_event::events::{Cache, CacheOp, CacheResult, Hardware, WhichCache};
///     # fn main() -> std::io::Result<()> {
///     // A `Cache` value representing L1 data cache read accesses.
///     const ACCESS: Cache = Cache {
///         which: WhichCache::L1D,
///         operation: CacheOp::READ,
///         result: CacheResult::ACCESS,
///     };
///
///     // A `Cache` value representing L1 data cache read misses.
///     const MISS: Cache = Cache { result: CacheResult::MISS, ..ACCESS };
///
///     // Construct a `Group` containing the two new counters, from which we
///     // can get counts over matching periods of time.
///     let mut group = Group::new()?;
///     let access_counter = Builder::new().group(&mut group).kind(ACCESS).build()?;
///     let miss_counter = Builder::new().group(&mut group).kind(MISS).build()?;
///     # Ok(()) }
///
/// [`which`]: enum.WhichCache.html
/// [`operation`]: enum.CacheOp.html
/// [`result`]: enum.CacheResult.html
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Cache {
    /// Which cache is being monitored? (data, instruction, ...)
    pub which: WhichCache,

    /// What operation is being monitored? (read, write, etc.)
    pub operation: CacheOp,

    /// All accesses, or just misses?
    pub result: CacheResult,
}

impl From<Cache> for Event {
    fn from(hw: Cache) -> Event {
        Event::Cache(hw)
    }
}

impl Cache {
    fn as_config(&self) -> u64 {
        self.which as u64 | ((self.operation as u64) << 8) | ((self.result as u64) << 16)
    }
}

/// A cache whose events we would like to count.
///
/// This is used in the `Cache` type as part of the identification of a cache
/// event. Each variant here corresponds to a particular
/// `PERF_COUNT_HW_CACHE_...` constant supported by the [`perf_event_open`][man]
/// system call.
///
/// [man]: http://man7.org/linux/man-pages/man2/perf_event_open.2.html
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum WhichCache {
    /// Level 1 data cache.
    L1D = bindings::PERF_COUNT_HW_CACHE_L1D,

    /// Level 1 instruction cache.
    L1I = bindings::PERF_COUNT_HW_CACHE_L1I,

    /// Last-level cache.
    LL = bindings::PERF_COUNT_HW_CACHE_LL,

    /// Data translation lookaside buffer (virtual address translation).
    DTLB = bindings::PERF_COUNT_HW_CACHE_DTLB,

    /// Instruction translation lookaside buffer (virtual address translation).
    ITLB = bindings::PERF_COUNT_HW_CACHE_ITLB,

    /// Branch prediction.
    BPU = bindings::PERF_COUNT_HW_CACHE_BPU,

    /// Memory accesses that stay local to the originating NUMA node.
    NODE = bindings::PERF_COUNT_HW_CACHE_NODE,
}

/// What sort of cache operation we would like to observe.
///
/// This is used in the `Cache` type as part of the identification of a cache
/// event. Each variant here corresponds to a particular
/// `PERF_COUNT_HW_CACHE_OP_...` constant supported by the
/// [`perf_event_open`][man] system call.
///
/// [man]: http://man7.org/linux/man-pages/man2/perf_event_open.2.html
#[repr(u32)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CacheOp {
    /// Read accesses.
    READ = bindings::PERF_COUNT_HW_CACHE_OP_READ,

    /// Write accesses.
    WRITE = bindings::PERF_COUNT_HW_CACHE_OP_WRITE,

    /// Prefetch accesses.
    PREFETCH = bindings::PERF_COUNT_HW_CACHE_OP_PREFETCH,
}

#[repr(u32)]
/// What sort of cache result we're interested in observing.
///
/// `ACCESS` counts the total number of operations performed on the cache,
/// whereas `MISS` counts only those requests that the cache could not satisfy.
/// Treating `MISS` as a fraction of `ACCESS` gives you the cache's miss rate.
///
/// This is used used in the `Cache` type as part of the identification of a
/// cache event. Each variant here corresponds to a particular
/// `PERF_COUNT_HW_CACHE_RESULT_...` constant supported by the
/// [`perf_event_open`][man] system call.
///
/// [man]: http://man7.org/linux/man-pages/man2/perf_event_open.2.html
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CacheResult {
    /// Cache was accessed.
    ACCESS = bindings::PERF_COUNT_HW_CACHE_RESULT_ACCESS,

    /// Cache access was a miss.
    MISS = bindings::PERF_COUNT_HW_CACHE_RESULT_MISS,
}

bitflags! {
    /// Memory access mask for a hardware data breakpoint.
    pub struct BreakpointAccess : u32 {
        /// Count when we read the memory location.
        const READ = bindings::HW_BREAKPOINT_R;

        /// Count when we write the memory location.
        const WRITE = bindings::HW_BREAKPOINT_W;

        /// Count when we read or write the memory location.
        const READ_WRITE = Self::READ.union(Self::WRITE).bits();
    }
}

/// A hardware breakpoint.
///
/// A hardware breakpoint watches a region of memory for accesses. It has three
/// parameters:
/// - the address that is being watched (`addr`)
/// - the number of bytes that breakpoint covers (`len`)
/// - which type of memory accesses we care about (`ty`)
///
/// Note that both number of bytes that can be watched as well as the number of
/// breakpoints that is allowed to be active at any given time is limited.
///
/// # Execute Breakpoint
/// We can use a breakpoint to count the number of times that a function gets
/// called, as long as the compiler does not optimize the function away.
///
/// ```
/// # use perf_event::Builder;
/// # use perf_event::events::Breakpoint;
/// #[inline(never)]
/// fn do_some_things() {
///     // ...
///     # println!("test println so the function doesn't get removed")
/// }
///
/// let fnptr = do_some_things as fn() as usize;
/// let mut counter = Builder::new()
///     .kind(Breakpoint::execute(fnptr as u64))
///     .build()?;
/// counter.enable()?;
///
/// for _ in 0..500 {
///     do_some_things();
/// }
///
/// counter.disable()?;
/// assert_eq!(counter.read()?, 500);
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// # Data Breakpoint
/// We can also use a breakpoint to count the number of times that a memory
/// location is accessed.
/// ```
/// # use perf_event::Builder;
/// # use perf_event::events::Breakpoint;
/// #
/// let mut data: Vec<u64> = (0..1024).rev().collect();
///
/// let mut counter = Builder::new()
///     .kind(Breakpoint::read_write(&data[20] as *const _ as usize as u64, 8))
///     .build()?;
/// counter.enable()?;
/// data.sort();
/// counter.disable()?;
///
/// println!("Position 20 accessed {} times", counter.read()?);
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// # Usage Notes
/// - Some systems do not support creating read-only or write-only breakpoints.
///   If you are getting `EINVAL` errors while trying to build such a counter
///   using a read-write breakpoint might work instead.
///
/// - The valid values of len are quite limited. The [`perf_event_open`][man]
///   manpage indicates that the only valid values for `bp_len` are 1, 2, 4,
///   and 8.
///
/// [man]: http://man7.org/linux/man-pages/man2/perf_event_open.2.html
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Breakpoint {
    /// Data breakpoint. Triggers when code reads or writes to the memory area
    /// as configured by the parameters below.
    Data {
        /// Bitfield containing the types of accesses we want the breakpoint to
        /// trigger on.
        access: BreakpointAccess,

        /// The address of the memory location on which the breakpoint should
        /// trigger.
        addr: u64,

        /// The length of the breakpoint being measured.
        ///
        /// There are a limited number of valid values for this field. Basically,
        /// the options are 1, 2, 4, and 8. Setting this field to anything else
        /// will cause counter creation to fail with an error.
        len: u64,
    },

    /// Code breakpoint. Triggers when the code at the address is executed.
    Code {
        /// The address that the breakpoint is monitoring.
        addr: u64,
    },
}

impl Breakpoint {
    /// Create a code execution breakpoint, that counts the number of
    /// times the instruction at the provided address was executed.
    pub const fn execute(addr: u64) -> Self {
        Self::Code { addr }
    }

    /// Create a memory read breakpoint, that counts the number of
    /// times we read from the provided memory location.
    ///
    /// See the struct field docs for valid values of `len`.
    pub const fn read(addr: u64, len: u64) -> Self {
        Self::Data {
            access: BreakpointAccess::READ,
            addr,
            len,
        }
    }

    /// Create a memory write breakpoint, that counts the number of
    /// times we write to the provided memory location.
    ///
    /// See the struct field docs for valid values of `len`.
    pub const fn write(addr: u64, len: u64) -> Self {
        Self::Data {
            access: BreakpointAccess::WRITE,
            addr,
            len,
        }
    }

    /// Create a memory access breakpoint, that counts the number of
    /// times we either read from or write to the provided memory
    /// location.
    ///
    /// See the struct field docs for valid values of `len`.
    pub const fn read_write(addr: u64, len: u64) -> Self {
        Self::Data {
            access: BreakpointAccess::READ_WRITE,
            addr,
            len,
        }
    }
}

impl From<Breakpoint> for Event {
    fn from(bp: Breakpoint) -> Self {
        Event::Breakpoint(bp)
    }
}
