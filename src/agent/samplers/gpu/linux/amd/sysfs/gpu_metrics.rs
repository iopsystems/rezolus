//! Parsers for the two sysfs files this sampler reads.
//!
//! `gpu_metrics` is a `struct gpu_metrics_v2_1` from the kernel's
//! `kgd_pp_interface.h`, 120 bytes in native (little-endian) order. Only that
//! format is accepted. The offsets below were confirmed against captures from
//! a Radeon 780M (SMU 13.0.4); other formats have no capture, so
//! [`GpuMetrics::parse`] refuses them rather than reading their fields at
//! these offsets.

/// `structure_size`, `format_revision` and `content_revision` of the accepted
/// format.
const SIZE: usize = 120;
const FORMAT_REVISION: u8 = 2;
const CONTENT_REVISION: u8 = 1;

const TEMPERATURE_GFX: usize = 4;
const TEMPERATURE_SOC: usize = 6;
const AVERAGE_GFX_ACTIVITY: usize = 28;
const AVERAGE_SOCKET_POWER: usize = 40;
const AVERAGE_SOC_POWER: usize = 44;
const AVERAGE_GFX_POWER: usize = 46;
const AVERAGE_GFXCLK: usize = 64;
const AVERAGE_SOCCLK: usize = 66;
const AVERAGE_UCLK: usize = 68;
const THROTTLE_STATUS: usize = 108;

/// The fields this sampler publishes, in the kernel's units.
///
/// The kernel fills a field the firmware does not provide with all ones, which
/// reads here as `None`. On the 780M that covers the six
/// `current_{gfx,soc,u,f,v,d}clk` fields and the CPU power field, which is why
/// those are not parsed.
///
/// `average_fclk_frequency` is not parsed because SMU 13.0.4 fills it from the
/// memory clock; the fabric clock comes from [`current_level_mhz`] instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuMetrics {
    pub temperature_gfx_centi_c: Option<u16>,
    pub temperature_soc_centi_c: Option<u16>,
    pub gfx_activity_centi_percent: Option<u16>,
    pub socket_power_mw: Option<u16>,
    pub soc_power_mw: Option<u16>,
    /// The `average_gfx_power` field. SMU 13.0.4 fills it from the VDD rail,
    /// which also carries other loads: it read 8.4 W with the GPU at 0%
    /// activity.
    pub vdd_power_mw: Option<u16>,
    pub gfxclk_mhz: Option<u16>,
    pub socclk_mhz: Option<u16>,
    pub uclk_mhz: Option<u16>,
    pub throttle_status: Option<u32>,
}

/// A `gpu_metrics` buffer this parser does not accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// Fewer bytes than the header, or than `structure_size` says.
    Short(usize),
    /// Any other format, as (`structure_size`, `format_revision`,
    /// `content_revision`).
    Unsupported(u16, u8, u8),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Short(n) => write!(f, "a short gpu_metrics read of {n} bytes"),
            Self::Unsupported(size, format, content) => {
                write!(f, "gpu_metrics format v{format}.{content}, {size} bytes")
            }
        }
    }
}

impl GpuMetrics {
    pub fn parse(buffer: &[u8]) -> Result<Self, FormatError> {
        if buffer.len() < 4 {
            return Err(FormatError::Short(buffer.len()));
        }
        let size = u16::from_le_bytes([buffer[0], buffer[1]]);
        let (format, content) = (buffer[2], buffer[3]);
        if (size as usize, format, content) != (SIZE, FORMAT_REVISION, CONTENT_REVISION) {
            return Err(FormatError::Unsupported(size, format, content));
        }
        if buffer.len() < SIZE {
            return Err(FormatError::Short(buffer.len()));
        }

        let u16_at = |offset: usize| {
            let value = u16::from_le_bytes([buffer[offset], buffer[offset + 1]]);
            (value != u16::MAX).then_some(value)
        };
        let throttle = u32::from_le_bytes([
            buffer[THROTTLE_STATUS],
            buffer[THROTTLE_STATUS + 1],
            buffer[THROTTLE_STATUS + 2],
            buffer[THROTTLE_STATUS + 3],
        ]);

        Ok(Self {
            temperature_gfx_centi_c: u16_at(TEMPERATURE_GFX),
            temperature_soc_centi_c: u16_at(TEMPERATURE_SOC),
            gfx_activity_centi_percent: u16_at(AVERAGE_GFX_ACTIVITY),
            socket_power_mw: u16_at(AVERAGE_SOCKET_POWER),
            soc_power_mw: u16_at(AVERAGE_SOC_POWER),
            vdd_power_mw: u16_at(AVERAGE_GFX_POWER),
            gfxclk_mhz: u16_at(AVERAGE_GFXCLK),
            socclk_mhz: u16_at(AVERAGE_SOCCLK),
            uclk_mhz: u16_at(AVERAGE_UCLK),
            throttle_status: (throttle != u32::MAX).then_some(throttle),
        })
    }
}

/// The frequency of the level marked current in a `pp_dpm_*` file, whose lines
/// read `2: 1960Mhz *`. `None` when no line carries the mark.
pub fn current_level_mhz(text: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let _level = fields.next()?;
        let frequency = fields.next()?;
        (fields.next() == Some("*"))
            .then(|| frequency.strip_suffix("Mhz")?.parse().ok())
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: &[u8] = include_bytes!("fixtures/gpu_metrics_v2_1_idle.bin");
    const LOAD: &[u8] = include_bytes!("fixtures/gpu_metrics_v2_1_load.bin");
    const EDC: &[u8] = include_bytes!("fixtures/gpu_metrics_v2_1_edc.bin");

    #[test]
    fn an_idle_780m_capture_decodes_to_its_low_clocks() {
        assert_eq!(
            GpuMetrics::parse(IDLE),
            Ok(GpuMetrics {
                temperature_gfx_centi_c: Some(3725),
                temperature_soc_centi_c: Some(3787),
                gfx_activity_centi_percent: Some(5),
                socket_power_mw: Some(18542),
                soc_power_mw: Some(1004),
                vdd_power_mw: Some(8367),
                gfxclk_mhz: Some(800),
                socclk_mhz: Some(400),
                uclk_mhz: Some(2800),
                throttle_status: Some(0),
            })
        );
    }

    #[test]
    fn a_loaded_780m_capture_decodes_to_its_peak_clocks() {
        assert_eq!(
            GpuMetrics::parse(LOAD),
            Ok(GpuMetrics {
                temperature_gfx_centi_c: Some(5050),
                temperature_soc_centi_c: Some(4125),
                gfx_activity_centi_percent: Some(9004),
                socket_power_mw: Some(33426),
                soc_power_mw: Some(4737),
                vdd_power_mw: Some(21038),
                gfxclk_mhz: Some(2700),
                socclk_mhz: Some(1028),
                uclk_mhz: Some(2800),
                throttle_status: Some(0),
            })
        );
    }

    #[test]
    fn a_capture_taken_during_an_edc_limit_keeps_the_throttle_bit() {
        let metrics = GpuMetrics::parse(EDC).unwrap();
        assert_eq!(metrics.throttle_status, Some(0x800));
    }

    #[test]
    fn a_field_of_all_ones_reads_as_absent() {
        let mut buffer = LOAD.to_vec();
        buffer[AVERAGE_SOCKET_POWER..AVERAGE_SOCKET_POWER + 2].fill(0xff);
        buffer[THROTTLE_STATUS..THROTTLE_STATUS + 4].fill(0xff);
        let metrics = GpuMetrics::parse(&buffer).unwrap();
        assert_eq!(metrics.socket_power_mw, None);
        assert_eq!(metrics.throttle_status, None);
    }

    /// The captures cannot show this offset is right: SMU 13.0.4 writes the
    /// same value to `average_uclk_frequency` and the fclk field after it.
    #[test]
    fn the_memory_clock_comes_from_the_uclk_field_and_not_the_fclk_field() {
        let mut buffer = LOAD.to_vec();
        // 68 and 70 are written out so the test does not follow the constant.
        buffer[68..70].copy_from_slice(&1000u16.to_le_bytes());
        buffer[70..72].copy_from_slice(&2000u16.to_le_bytes());
        assert_eq!(GpuMetrics::parse(&buffer).unwrap().uclk_mhz, Some(1000));
    }

    #[test]
    fn another_content_revision_is_refused() {
        let mut buffer = LOAD.to_vec();
        buffer[3] = 2;
        assert_eq!(
            GpuMetrics::parse(&buffer),
            Err(FormatError::Unsupported(120, 2, 2))
        );
    }

    #[test]
    fn another_structure_size_is_refused() {
        let mut buffer = LOAD.to_vec();
        buffer[0] = 124;
        assert_eq!(
            GpuMetrics::parse(&buffer),
            Err(FormatError::Unsupported(124, 2, 1))
        );
    }

    #[test]
    fn a_short_read_is_an_error_and_does_not_panic() {
        assert_eq!(GpuMetrics::parse(&LOAD[..3]), Err(FormatError::Short(3)));
        assert_eq!(
            GpuMetrics::parse(&LOAD[..119]),
            Err(FormatError::Short(119))
        );
    }

    #[test]
    fn the_marked_dpm_level_is_the_current_frequency() {
        let text = "0: 500Mhz \n1: 1400Mhz *\n2: 1960Mhz \n";
        assert_eq!(current_level_mhz(text), Some(1400));
    }

    #[test]
    fn a_dpm_file_with_no_marked_level_has_no_current_frequency() {
        assert_eq!(current_level_mhz("0: 500Mhz \n1: 1960Mhz \n"), None);
    }
}
