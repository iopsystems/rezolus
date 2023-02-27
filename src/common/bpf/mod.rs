use std::collections::HashMap;
use crate::Instant;

pub const KEYS: &[u8] = &[
	0, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 5, 0, 0, 0, 6,
	0, 0, 0, 7, 0, 0, 0, 8, 0, 0, 0, 9, 0, 0, 0, 10, 0, 0, 0, 11, 0, 0, 0, 12,
	0, 0, 0, 13, 0, 0, 0, 14, 0, 0, 0, 15, 0, 0, 0, 16, 0, 0, 0, 17, 0, 0, 0,
	18, 0, 0, 0, 19, 0, 0, 0, 20, 0, 0, 0, 21, 0, 0, 0, 22, 0, 0, 0, 23, 0, 0,
	0, 24, 0, 0, 0, 25, 0, 0, 0, 26, 0, 0, 0, 27, 0, 0, 0, 28, 0, 0, 0, 29, 0,
	0, 0, 30, 0, 0, 0, 31, 0, 0, 0, 32, 0, 0, 0, 33, 0, 0, 0, 34, 0, 0, 0, 35,
	0, 0, 0, 36, 0, 0, 0, 37, 0, 0, 0, 38, 0, 0, 0, 39, 0, 0, 0, 40, 0, 0, 0,
	41, 0, 0, 0, 42, 0, 0, 0, 43, 0, 0, 0, 44, 0, 0, 0, 45, 0, 0, 0, 46, 0, 0,
	0, 47, 0, 0, 0, 48, 0, 0, 0, 49, 0, 0, 0, 50, 0, 0, 0, 51, 0, 0, 0, 52, 0,
	0, 0, 53, 0, 0, 0, 54, 0, 0, 0, 55, 0, 0, 0, 56, 0, 0, 0, 57, 0, 0, 0, 58,
	0, 0, 0, 59, 0, 0, 0, 60, 0, 0, 0, 61, 0, 0, 0, 62, 0, 0, 0, 63, 0, 0, 0,
	64, 0, 0, 0, 65, 0, 0, 0, 66, 0, 0, 0, 67, 0, 0, 0, 68, 0, 0, 0, 69, 0, 0,
	0, 70, 0, 0, 0, 71, 0, 0, 0, 72, 0, 0, 0, 73, 0, 0, 0, 74, 0, 0, 0, 75, 0,
	0, 0, 76, 0, 0, 0, 77, 0, 0, 0, 78, 0, 0, 0, 79, 0, 0, 0, 80, 0, 0, 0, 81,
	0, 0, 0, 82, 0, 0, 0, 83, 0, 0, 0, 84, 0, 0, 0, 85, 0, 0, 0, 86, 0, 0, 0,
	87, 0, 0, 0, 88, 0, 0, 0, 89, 0, 0, 0, 90, 0, 0, 0, 91, 0, 0, 0, 92, 0, 0,
	0, 93, 0, 0, 0, 94, 0, 0, 0, 95, 0, 0, 0, 96, 0, 0, 0, 97, 0, 0, 0, 98, 0,
	0, 0, 99, 0, 0, 0, 100, 0, 0, 0, 101, 0, 0, 0, 102, 0, 0, 0, 103, 0, 0, 0,
	104, 0, 0, 0, 105, 0, 0, 0, 106, 0, 0, 0, 107, 0, 0, 0, 108, 0, 0, 0, 109,
	0, 0, 0, 110, 0, 0, 0, 111, 0, 0, 0, 112, 0, 0, 0, 113, 0, 0, 0, 114, 0, 0,
	0, 115, 0, 0, 0, 116, 0, 0, 0, 117, 0, 0, 0, 118, 0, 0, 0, 119, 0, 0, 0,
	120, 0, 0, 0, 121, 0, 0, 0, 122, 0, 0, 0, 123, 0, 0, 0, 124, 0, 0, 0, 125,
	0, 0, 0, 126, 0, 0, 0, 127, 0, 0, 0, 128, 0, 0, 0, 129, 0, 0, 0, 130, 0, 0,
	0, 131, 0, 0, 0, 132, 0, 0, 0, 133, 0, 0, 0, 134, 0, 0, 0, 135, 0, 0, 0,
	136, 0, 0, 0, 137, 0, 0, 0, 138, 0, 0, 0, 139, 0, 0, 0, 140, 0, 0, 0, 141,
	0, 0, 0, 142, 0, 0, 0, 143, 0, 0, 0, 144, 0, 0, 0, 145, 0, 0, 0, 146, 0, 0,
	0, 147, 0, 0, 0, 148, 0, 0, 0, 149, 0, 0, 0, 150, 0, 0, 0, 151, 0, 0, 0,
	152, 0, 0, 0, 153, 0, 0, 0, 154, 0, 0, 0, 155, 0, 0, 0, 156, 0, 0, 0, 157,
	0, 0, 0, 158, 0, 0, 0, 159, 0, 0, 0, 160, 0, 0, 0, 161, 0, 0, 0, 162, 0, 0,
	0, 163, 0, 0, 0, 164, 0, 0, 0, 165, 0, 0, 0, 166, 0, 0, 0, 167, 0, 0, 0,
	168, 0, 0, 0, 169, 0, 0, 0, 170, 0, 0, 0, 171, 0, 0, 0, 172, 0, 0, 0, 173,
	0, 0, 0, 174, 0, 0, 0, 175, 0, 0, 0, 176, 0, 0, 0, 177, 0, 0, 0, 178, 0, 0,
	0, 179, 0, 0, 0, 180, 0, 0, 0, 181, 0, 0, 0, 182, 0, 0, 0, 183, 0, 0, 0,
	184, 0, 0, 0, 185, 0, 0, 0, 186, 0, 0, 0, 187, 0, 0, 0, 188, 0, 0, 0, 189,
	0, 0, 0, 190, 0, 0, 0, 191, 0, 0, 0, 192, 0, 0, 0, 193, 0, 0, 0, 194, 0, 0,
	0, 195, 0, 0, 0, 196, 0, 0, 0, 197, 0, 0, 0, 198, 0, 0, 0, 199, 0, 0, 0,
	200, 0, 0, 0, 201, 0, 0, 0, 202, 0, 0, 0, 203, 0, 0, 0, 204, 0, 0, 0, 205,
	0, 0, 0, 206, 0, 0, 0, 207, 0, 0, 0, 208, 0, 0, 0, 209, 0, 0, 0, 210, 0, 0,
	0, 211, 0, 0, 0, 212, 0, 0, 0, 213, 0, 0, 0, 214, 0, 0, 0, 215, 0, 0, 0,
	216, 0, 0, 0, 217, 0, 0, 0, 218, 0, 0, 0, 219, 0, 0, 0, 220, 0, 0, 0, 221,
	0, 0, 0, 222, 0, 0, 0, 223, 0, 0, 0, 224, 0, 0, 0, 225, 0, 0, 0, 226, 0, 0,
	0, 227, 0, 0, 0, 228, 0, 0, 0, 229, 0, 0, 0, 230, 0, 0, 0, 231, 0, 0, 0,
	232, 0, 0, 0, 233, 0, 0, 0, 234, 0, 0, 0, 235, 0, 0, 0, 236, 0, 0, 0, 237,
	0, 0, 0, 238, 0, 0, 0, 239, 0, 0, 0, 240, 0, 0, 0, 241, 0, 0, 0, 242, 0, 0,
	0, 243, 0, 0, 0, 244, 0, 0, 0, 245, 0, 0, 0, 246, 0, 0, 0, 247, 0, 0, 0,
	248, 0, 0, 0, 249, 0, 0, 0, 250, 0, 0, 0, 251, 0, 0, 0, 252, 0, 0, 0, 253,
	0, 0, 0, 254, 0, 0, 0, 255, 0, 0, 0, 0, 1, 0, 0, 1, 1, 0, 0, 2, 1, 0, 0, 3,
	1, 0, 0, 4, 1, 0, 0, 5, 1, 0, 0, 6, 1, 0, 0, 7, 1, 0, 0, 8, 1, 0, 0, 9, 1,
	0, 0, 10, 1, 0, 0, 11, 1, 0, 0, 12, 1, 0, 0, 13, 1, 0, 0, 14, 1, 0, 0, 15,
	1, 0, 0, 16, 1, 0, 0, 17, 1, 0, 0, 18, 1, 0, 0, 19, 1, 0, 0, 20, 1, 0, 0,
	21, 1, 0, 0, 22, 1, 0, 0, 23, 1, 0, 0, 24, 1, 0, 0, 25, 1, 0, 0, 26, 1, 0,
	0, 27, 1, 0, 0, 28, 1, 0, 0, 29, 1, 0, 0, 30, 1, 0, 0, 31, 1, 0, 0, 32, 1,
	0, 0, 33, 1, 0, 0, 34, 1, 0, 0, 35, 1, 0, 0, 36, 1, 0, 0, 37, 1, 0, 0, 38,
	1, 0, 0, 39, 1, 0, 0, 40, 1, 0, 0, 41, 1, 0, 0, 42, 1, 0, 0, 43, 1, 0, 0,
	44, 1, 0, 0, 45, 1, 0, 0, 46, 1, 0, 0, 47, 1, 0, 0, 48, 1, 0, 0, 49, 1, 0,
	0, 50, 1, 0, 0, 51, 1, 0, 0, 52, 1, 0, 0, 53, 1, 0, 0, 54, 1, 0, 0, 55, 1,
	0, 0, 56, 1, 0, 0, 57, 1, 0, 0, 58, 1, 0, 0, 59, 1, 0, 0, 60, 1, 0, 0, 61,
	1, 0, 0, 62, 1, 0, 0, 63, 1, 0, 0, 64, 1, 0, 0, 65, 1, 0, 0, 66, 1, 0, 0,
	67, 1, 0, 0, 68, 1, 0, 0, 69, 1, 0, 0, 70, 1, 0, 0, 71, 1, 0, 0, 72, 1, 0,
	0, 73, 1, 0, 0, 74, 1, 0, 0, 75, 1, 0, 0, 76, 1, 0, 0, 77, 1, 0, 0, 78, 1,
	0, 0, 79, 1, 0, 0, 80, 1, 0, 0, 81, 1, 0, 0, 82, 1, 0, 0, 83, 1, 0, 0, 84,
	1, 0, 0, 85, 1, 0, 0, 86, 1, 0, 0, 87, 1, 0, 0, 88, 1, 0, 0, 89, 1, 0, 0,
	90, 1, 0, 0, 91, 1, 0, 0, 92, 1, 0, 0, 93, 1, 0, 0, 94, 1, 0, 0, 95, 1, 0,
	0, 96, 1, 0, 0, 97, 1, 0, 0, 98, 1, 0, 0, 99, 1, 0, 0, 100, 1, 0, 0, 101, 1,
	0, 0, 102, 1, 0, 0, 103, 1, 0, 0, 104, 1, 0, 0, 105, 1, 0, 0, 106, 1, 0, 0,
	107, 1, 0, 0, 108, 1, 0, 0, 109, 1, 0, 0, 110, 1, 0, 0, 111, 1, 0, 0, 112,
	1, 0, 0, 113, 1, 0, 0, 114, 1, 0, 0, 115, 1, 0, 0, 116, 1, 0, 0, 117, 1, 0,
	0, 118, 1, 0, 0, 119, 1, 0, 0, 120, 1, 0, 0, 121, 1, 0, 0, 122, 1, 0, 0,
	123, 1, 0, 0, 124, 1, 0, 0, 125, 1, 0, 0, 126, 1, 0, 0, 127, 1, 0, 0, 128,
	1, 0, 0, 129, 1, 0, 0, 130, 1, 0, 0, 131, 1, 0, 0, 132, 1, 0, 0, 133, 1, 0,
	0, 134, 1, 0, 0, 135, 1, 0, 0, 136, 1, 0, 0, 137, 1, 0, 0, 138, 1, 0, 0,
	139, 1, 0, 0, 140, 1, 0, 0, 141, 1, 0, 0, 142, 1, 0, 0, 143, 1, 0, 0, 144,
	1, 0, 0, 145, 1, 0, 0, 146, 1, 0, 0, 147, 1, 0, 0, 148, 1, 0, 0, 149, 1, 0,
	0, 150, 1, 0, 0, 151, 1, 0, 0, 152, 1, 0, 0, 153, 1, 0, 0, 154, 1, 0, 0,
	155, 1, 0, 0, 156, 1, 0, 0, 157, 1, 0, 0, 158, 1, 0, 0, 159, 1, 0, 0, 160,
	1, 0, 0, 161, 1, 0, 0, 162, 1, 0, 0, 163, 1, 0, 0, 164, 1, 0, 0, 165, 1, 0,
	0, 166, 1, 0, 0, 167, 1, 0, 0, 168, 1, 0, 0, 169, 1, 0, 0, 170, 1, 0, 0,
	171, 1, 0, 0, 172, 1, 0, 0, 173, 1, 0, 0, 174, 1, 0, 0, 175, 1, 0, 0, 176,
	1, 0, 0, 177, 1, 0, 0, 178, 1, 0, 0, 179, 1, 0, 0, 180, 1, 0, 0, 181, 1, 0,
	0, 182, 1, 0, 0, 183, 1, 0, 0, 184, 1, 0, 0, 185, 1, 0, 0, 186, 1, 0, 0,
	187, 1, 0, 0, 188, 1, 0, 0, 189, 1, 0, 0, 190, 1, 0, 0, 191, 1, 0, 0, 192,
	1, 0, 0, 193, 1, 0, 0, 194, 1, 0, 0, 195, 1, 0, 0, 196, 1, 0, 0, 197, 1, 0,
	0, 198, 1, 0, 0, 199, 1, 0, 0, 200, 1, 0, 0, 201, 1, 0, 0, 202, 1, 0, 0,
	203, 1, 0, 0, 204, 1, 0, 0, 205, 1, 0, 0, 206, 1, 0, 0, 207, 1, 0, 0, 208,
	1, 0, 0, 209, 1, 0, 0, 210, 1, 0, 0, 211, 1, 0, 0, 212, 1, 0, 0, 213, 1, 0,
	0, 214, 1, 0, 0, 215, 1, 0, 0, 216, 1, 0, 0, 217, 1, 0, 0, 218, 1, 0, 0,
	219, 1, 0, 0, 220, 1, 0, 0, 221, 1, 0, 0, 222, 1, 0, 0, 223, 1, 0, 0, 224,
	1, 0, 0, 225, 1, 0, 0, 226, 1, 0, 0, 227, 1, 0, 0, 228, 1, 0, 0, 229, 1, 0,
	0, 230, 1, 0, 0, 231, 1, 0, 0, 232, 1, 0, 0, 233, 1, 0, 0, 234, 1, 0, 0,
	235, 1, 0, 0, 236, 1, 0, 0, 237, 1, 0, 0, 238, 1, 0, 0, 239, 1, 0, 0];

#[cfg(feature = "bpf")]
/// This function converts indices back to values for rustcommon histogram with
/// the parameters `m = 0`, `r = 4`, `n = 64`. This covers the entire range from
/// 1 to u64::MAX and uses 496 buckets per histogram, which works out to ~4KB
/// for each histogram. In userspace we will likely have 61 histograms -
/// bringing the total to ~256KB per stat.
pub fn key_to_value(index: u64) -> u64 {
    let g = index >> 3;
    let b = index - g * 8 + 1;

    if g < 1 {
        b - 1
    } else {
        (1 << (2 + g)) + (1 << (g - 1)) * b - 1
    }
}

pub fn update_histogram_from_dist(fd: i32, stat: &metriken::Lazy<metriken::Heatmap>, previous: &mut [u64]) {
	let now = Instant::now();

	let opts = libbpf_sys::bpf_map_batch_opts {
        sz: 24 as libbpf_sys::size_t,
        elem_flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
        flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
    };

	let mut keys = KEYS.to_owned();
    let mut out: Vec<u8> = vec![0; 496 * 8];
    let mut nkeys: u32 = 496;

    let in_batch = std::ptr::null_mut();
    let mut out_batch = 0_u32;

    let ret = unsafe {
        libbpf_sys::bpf_map_lookup_batch(
            fd,
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

// note: 
pub fn read_counters(fd: i32, count: usize) -> HashMap<usize, u64> {
	let mut result = HashMap::with_capacity(count);

	let opts = libbpf_sys::bpf_map_batch_opts {
        sz: 24 as libbpf_sys::size_t,
        elem_flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
        flags: libbpf_sys::BPF_ANY as libbpf_sys::__u64,
    };
            
	let mut keys = Vec::with_capacity(count * 4);
	for k in 0..count {
		keys.extend_from_slice(&k.to_ne_bytes());
	}
    let mut out: Vec<u8> = vec![0; count * 8];
    let mut nkeys: u32 = count as _;

    let in_batch = std::ptr::null_mut();
    let mut out_batch = 0_u32;

    let ret = unsafe {
        libbpf_sys::bpf_map_lookup_batch(
            fd,
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

        result.insert(k, c);
    }

    result
}
