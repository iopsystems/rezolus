//! The NVMe SMART / Health Information log page (0x02), decoded. Shared by
//! the Linux sampler (read through the admin passthrough ioctl) and the macOS
//! sampler (read through IOKit's NVMe SMART user client).

/// Parse an NVMe SMART / Health log page (0x02). Composite Temperature is a
/// little-endian `u16` at bytes 1..3, in Kelvin. Returns °C, or `None` if the
/// buffer is too short or the field is 0 (not reported).
pub fn parse_smart_temperature(buf: &[u8]) -> Option<i64> {
    if buf.len() < 3 {
        return None;
    }
    let kelvin = u16::from_le_bytes([buf[1], buf[2]]) as i64;
    if kelvin == 0 {
        return None; // field not reported
    }
    Some(kelvin - 273)
}

/// Thermal-throttling telemetry decoded from the NVMe SMART / Health log page
/// (0x02). All counters are monotonic, so a coarse read cadence loses no events.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NvmeHealth {
    /// Composite temperature in °C (`None` if not reported).
    pub temperature_c: Option<i64>,
    /// Cumulative seconds the composite temperature was ≥ the warning threshold
    /// (WCTEMP). Always maintained by the controller.
    pub warning_temp_time_s: u64,
    /// Cumulative seconds ≥ the critical threshold (CCTEMP).
    pub critical_temp_time_s: u64,
    /// Host-thermal-management transition counts for TMT1/TMT2. Only nonzero
    /// when Host-Controlled Thermal Management is enabled (Set Features 0x10).
    pub thermal_mgmt_transitions: [u64; 2],
    /// Cumulative seconds spent in the TMT1/TMT2 thermal-management states.
    pub thermal_mgmt_time_s: [u64; 2],
}

/// Parse the NVMe SMART / Health log page (0x02) into [`NvmeHealth`]. Field
/// offsets per the NVMe Base Spec: Warning/Critical Composite Temperature Time
/// (192/196, u32 minutes → seconds), Thermal Management Temperature transition
/// counts (216/220, u32) and total times (224/228, u32 seconds). Returns `None`
/// if the buffer is shorter than the last field used.
pub fn parse_health(buf: &[u8]) -> Option<NvmeHealth> {
    // Last field used is Total Time For TMT2 at offset 228..232.
    if buf.len() < 232 {
        return None;
    }
    let u32le = |o: usize| u32::from_le_bytes([buf[o], buf[o + 1], buf[o + 2], buf[o + 3]]) as u64;

    Some(NvmeHealth {
        temperature_c: parse_smart_temperature(buf),
        warning_temp_time_s: u32le(192) * 60,
        critical_temp_time_s: u32le(196) * 60,
        thermal_mgmt_transitions: [u32le(216), u32le(220)],
        thermal_mgmt_time_s: [u32le(224), u32le(228)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SMART log buffer with Composite Temperature set to `kelvin`.
    fn smart_log(kelvin: u16) -> Vec<u8> {
        let mut buf = vec![0u8; 512];
        let [lo, hi] = kelvin.to_le_bytes();
        buf[1] = lo;
        buf[2] = hi;
        buf
    }

    #[test]
    fn converts_kelvin_to_celsius() {
        // 311 K = 38 C.
        assert_eq!(parse_smart_temperature(&smart_log(311)), Some(38));
    }

    #[test]
    fn zero_field_is_none() {
        assert_eq!(parse_smart_temperature(&smart_log(0)), None);
    }

    #[test]
    fn too_short_is_none() {
        assert_eq!(parse_smart_temperature(&[0u8; 2]), None);
    }

    /// Build a 512-byte SMART log with the thermal fields set.
    fn health_log(
        kelvin: u16,
        warn_min: u32,
        crit_min: u32,
        tmt: [(u32, u32); 2], // (transition_count, total_time_s) for TMT1/TMT2
    ) -> Vec<u8> {
        let mut buf = vec![0u8; 512];
        buf[1..3].copy_from_slice(&kelvin.to_le_bytes());
        buf[192..196].copy_from_slice(&warn_min.to_le_bytes());
        buf[196..200].copy_from_slice(&crit_min.to_le_bytes());
        buf[216..220].copy_from_slice(&tmt[0].0.to_le_bytes());
        buf[220..224].copy_from_slice(&tmt[1].0.to_le_bytes());
        buf[224..228].copy_from_slice(&tmt[0].1.to_le_bytes());
        buf[228..232].copy_from_slice(&tmt[1].1.to_le_bytes());
        buf
    }

    #[test]
    fn parses_thermal_throttle_fields() {
        // 320 K = 47 C; 3 min warning, 1 min critical; TMT1 5 transitions/120 s,
        // TMT2 2 transitions/30 s.
        let buf = health_log(320, 3, 1, [(5, 120), (2, 30)]);
        let h = parse_health(&buf).expect("parse");
        assert_eq!(h.temperature_c, Some(47));
        assert_eq!(h.warning_temp_time_s, 180); // 3 min × 60
        assert_eq!(h.critical_temp_time_s, 60); // 1 min × 60
        assert_eq!(h.thermal_mgmt_transitions, [5, 2]);
        assert_eq!(h.thermal_mgmt_time_s, [120, 30]);
    }

    #[test]
    fn health_all_zero_is_clean() {
        let h = parse_health(&vec![0u8; 512]).expect("parse");
        assert_eq!(h.temperature_c, None); // 0 K = not reported
        assert_eq!(h.warning_temp_time_s, 0);
        assert_eq!(h.thermal_mgmt_transitions, [0, 0]);
    }

    #[test]
    fn health_too_short_is_none() {
        assert_eq!(parse_health(&[0u8; 100]), None);
    }
}
