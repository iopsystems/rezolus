pub mod events;

mod sys;

use events::Event;
use sys::perf_event_attr;
use std::fs::File;
use std::os::unix::raw::pid_t;

#[derive(Debug)]
enum EventPid<'a> {
    /// Monitor the calling process.
    ThisProcess,

    /// Monitor the given pid.
    Other(pid_t),

    /// Monitor members of the given cgroup.
    CGroup(&'a File),

    /// Monitor any process on some given CPU.
    Any,
}

pub struct PerfCounterBuilder<'a> {
    attrs: perf_event_attr,
    who: EventPid<'a>,
    cpu: Option<usize>,
}

impl<'a> PerfCounterBuilder<'a> {
	pub fn new<T: Into<Event>>(event: T) -> Self {
		let mut attrs = perf_event_attr {
            // Setting `size` accurately will not prevent the code from working
            // on older kernels. The module comments for `perf_event_open_sys`
            // explain why in far too much detail.
            size: std::mem::size_of::<perf_event_attr>() as u32,
            ..perf_event_attr::default()
        };

        attrs.set_disabled(1);

        // Request data for `time_enabled` and `time_running`.
        attrs.read_format |= sys::PERF_FORMAT_TOTAL_TIME_ENABLED as u64
            | sys::PERF_FORMAT_TOTAL_TIME_RUNNING as u64;

        Self {
            attrs,
            who: EventPid::Any,
            cpu: None,
        }
	}

	pub fn set_disabled(mut self, disabled: bool) -> Self {
		if disabled {
			self.attrs.set_disabled(1)
		} else {
			self.attrs.set_disabled(0)
		}

		self
	}
}

pub struct PerfCounterGroupBuilder<'a> {
	who: EventPid<'a>,
	cpu: Option<usize>,
	events: Vec<PerfCounterBuilder<'a>>,
}

impl<'a> PerfCounterGroupBuilder<'a> {
	pub fn new<T: Into<Event>>(leader: T) -> Self {
        Self {
        	who: EventPid::Any,
        	cpu: None,
        	events: vec![PerfCounterBuilder::new(leader)],
        }
	}
}