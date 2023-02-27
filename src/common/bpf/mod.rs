
#[cfg(feature = "bpf")]
/// This function converts indices back to values for rustcommon histogram with
/// the parameters `m = 0`, `r = 4`, `n = 64`. This covers the entire range from
/// 1 to u64::MAX and uses 496 buckets per histogram, which works out to ~4KB
pub fn key_to_value(index: u64) -> u64 {
    let g = index >> 3;
    let b = index - g * 8 + 1;

    if g < 1 {
        b - 1
    } else {
        (1 << (2 + g)) + (1 << (g - 1)) * b - 1
    }
}
