use std::collections::HashMap;
use crate::Instant;

mod keys;

pub use keys::KEYS;

use libbpf_rs::Map;

#[cfg(feature = "bpf")]
/// This function converts indices back to values for rustcommon histogram with
/// the parameters `m = 0`, `r = 8`, `n = 64`. This covers the entire range from
/// 1 to u64::MAX and uses 7424 buckets per histogram, which works out to 58KB
/// for each histogram in kernelspace (64bit counters). In userspace, we will
/// we will likely have 61 histograms => 1769KB per stat in userspace.
pub fn key_to_value(index: u64) -> u64 {
	// g = index >> (r - m - 1)
    let g = index >> 7;
    // b = index - g * G + 1
    let b = index - g * 128 + 1;

    if g < 1 {
    	// (1 << m) * b - 1
        b - 1
    } else {
    	// (1 << (r - 2 + g)) + (1 << (m + g - 1)) * b - 1
        (1 << (6 + g)) + (1 << (g - 1)) * b - 1
    }
}

pub fn update_histogram_from_dist(map: &libbpf_rs::Map, stat: &metriken::Lazy<metriken::Heatmap>, previous: &mut [u64]) {
	let now = Instant::now();

	let opts = libbpf_sys::bpf_map_batch_opts {
        sz: 24 as libbpf_sys::size_t,
        elem_flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
        flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
    };

	let mut keys = KEYS.to_owned();
    let mut out: Vec<u8> = vec![0; 7424 * 8];
    let mut nkeys: u32 = 7424;
    keys.truncate(nkeys as usize * 4);

    let in_batch = std::ptr::null_mut();
    let mut out_batch = 0_u32;

    let ret = unsafe {
        libbpf_sys::bpf_map_lookup_batch(
            map.fd(),
            in_batch as *mut core::ffi::c_void,
            &mut out_batch as *mut _ as *mut core::ffi::c_void,
            keys.as_mut_ptr() as *mut core::ffi::c_void,
            out.as_mut_ptr() as *mut core::ffi::c_void,
            &mut nkeys as *mut libbpf_sys::__u32,
            &opts as *const libbpf_sys::bpf_map_batch_opts,
        )
    };

    let nkeys = nkeys as usize;

    if ret == 0 {
        unsafe {
            out.set_len(8 * nkeys);
            keys.set_len(4 * nkeys);
        }
    } else {
        return;
    }

    let mut key = [0; 4];
    let mut current = [0; 8];

    for i in 0..nkeys {
        key.copy_from_slice(&keys[(i * 4)..((i + 1) * 4)]);
        current.copy_from_slice(&out[(i * 8)..((i + 1) * 8)]);

        let k = u32::from_ne_bytes(key) as usize;
        let c = u64::from_ne_bytes(current);

        let delta = c.wrapping_sub(previous[k]);
        previous[k] = c;

        if delta > 0 {
            let value = key_to_value(k as u64);
            stat.increment(now, value as _, delta as _);
        }
    }
}

// note: this reads multiple counters from a normal map array. This allows fewer
// syscalls, as we can make a single syscall for a whole batch of counters.
// however, it's more expensive in the bpf program to update (likely) contended
// atomics
pub fn read_counters(map: &libbpf_rs::Map, count: usize) -> Vec<u64> {
	let mut result = vec![0; count];

	let opts = libbpf_sys::bpf_map_batch_opts {
        sz: 24 as libbpf_sys::size_t,
        elem_flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
        flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
    };
            
	let mut keys = KEYS[0..(count * 4)].to_owned();
    let mut out: Vec<u8> = vec![0; count * 8];
    let mut nkeys: u32 = count as _;
    keys.truncate(nkeys as usize * 4);

    let in_batch = std::ptr::null_mut();
    let mut out_batch = 0_u32;

    let ret = unsafe {
        libbpf_sys::bpf_map_lookup_batch(
            map.fd(),
            in_batch as *mut core::ffi::c_void,
            &mut out_batch as *mut _ as *mut core::ffi::c_void,
            keys.as_mut_ptr() as *mut core::ffi::c_void,
            out.as_mut_ptr() as *mut core::ffi::c_void,
            &mut nkeys as *mut libbpf_sys::__u32,
            &opts as *const libbpf_sys::bpf_map_batch_opts,
        )
    };

    let nkeys = nkeys as usize;

    if ret == 0 {
        unsafe {
            out.set_len(8 * nkeys);
            keys.set_len(4 * nkeys);
        }
    } else {
        return result;
    }

    let mut key = [0; 4];
    let mut current = [0; 8];

    for i in 0..nkeys {
        key.copy_from_slice(&keys[(i * 4)..((i + 1) * 4)]);
        current.copy_from_slice(&out[(i * 8)..((i + 1) * 8)]);

        let k = u32::from_ne_bytes(key) as usize;
        let c = u64::from_ne_bytes(current);
        if let Some(v) = result.get_mut(k) {
        	*v = c;
        }
    }

    result
}

// reads a single counter from a percpu array map
//
// downside to this is we need to issue one syscall for each counter, but the
// updates within the bpf program will not be contended
pub fn read_percpu_counter(map: &libbpf_rs::Map, buf: &mut Vec<u8>) -> Result<u64, ()> {
	let num_cpu = libbpf_rs::num_possible_cpus().expect("failed to get number of cpus");

	let mut result: u64 = 0;

	let key = [0x00, 0x00, 0x00, 0x00];

    // let mut out: Vec<u8> = vec![0; num_cpu * 8];
    buf.clear();
    buf.resize(num_cpu * 8, 0);

    let ret = unsafe {
        libbpf_sys::bpf_map_lookup_elem(
            map.fd(),
            key.as_ptr() as *mut core::ffi::c_void,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
        )
    };

    if ret != 0 {
    	println!("ret: {ret}");
        return Err(());
    }

    let mut current = [0; 8];

	for i in 0..num_cpu {
        current.copy_from_slice(&buf[(i * 8)..((i + 1) * 8)]);

        result = result.wrapping_add(u64::from_ne_bytes(current));
    }

    Ok(result)
}

pub struct Counter {
    previous: Option<u64>,
    counter: &'static metriken::Lazy<metriken::Counter>,
    heatmap: Option<&'static metriken::Lazy<metriken::Heatmap>>,
    map: &'static Map,
    buf: Vec<u8>,
}

impl Counter {
    pub fn new(map: &'static Map, counter: &'static metriken::Lazy<metriken::Counter>, heatmap: Option<&'static metriken::Lazy<metriken::Heatmap>>) -> Self {
        Self {
            previous: None,
            counter,
            heatmap,
            map,
            buf: Vec::new(),
        }
    }

    // updates the counter by reading from it's associated map
    pub fn update(&mut self, now: Instant, elapsed_s: f64) {
    	// let map = obj.map(self.map).unwrap();
    	if let Ok(current) = crate::common::bpf::read_percpu_counter(self.map, &mut self.buf) {
	        if let Some(previous) = self.previous {
	            let delta = current.wrapping_sub(previous);
	            self.counter.add(delta);
	            if let Some(heatmap) = self.heatmap {
	                heatmap.increment(now, (delta as f64 / elapsed_s) as _, 1);
	            }
	        }
	        self.previous = Some(current);
	    }
    }

    // updates the counter by setting it to a specific value
    pub fn set(&mut self, now: Instant, elapsed_s: f64, current: u64) {
        if let Some(previous) = self.previous {
            let delta = current.wrapping_sub(previous);
            self.counter.add(delta);
            if let Some(heatmap) = self.heatmap {
                heatmap.increment(now, (delta as f64 / elapsed_s) as _, 1);
            }
        }
        self.previous = Some(current);
    }
}

pub struct Distribution {
    previous: [u64; 7424],
    heatmap: &'static metriken::Lazy<metriken::Heatmap>,
    map: &'static str,
}

impl Distribution {
    pub const fn new(map: &'static str, heatmap: &'static metriken::Lazy<metriken::Heatmap>) -> Self {
        Self {
            previous: [0; 7424],
            heatmap,
            map,
        }
    }

    pub fn update(&mut self, obj: &libbpf_rs::Object) {
        let map = obj.map(self.map).unwrap();
        crate::common::bpf::update_histogram_from_dist(map, self.heatmap, &mut self.previous);
    }
}