use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

/// Per-family lifetime identity cap. Slots are never reused in one process.
pub const MAX_SENSORS: usize = 256;

// The sampler the groups belong to: `hw_sensors` where a sampler compiles this
// file in its own module (Linux and macOS), `unattributed` elsewhere.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const GROUP_SAMPLER: &str = "hw_sensors";
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const GROUP_SAMPLER: &str = crate::agent::samplers::bpf_sampler_name("hw_sensors");

macro_rules! acquisition_group {
    ($static:ident, $registration:ident, $name:literal) => {
        pub static $static: AcquisitionGroup = AcquisitionGroup::new(GROUP_SAMPLER, $name);
        #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
        static $registration: &'static AcquisitionGroup = &$static;
    };
}

acquisition_group!(
    SENSOR_TEMPERATURE_ACQ,
    SENSOR_TEMPERATURE_ACQ_REG,
    "hw_sensors_temperature"
);
acquisition_group!(SENSOR_POWER_ACQ, SENSOR_POWER_ACQ_REG, "hw_sensors_power");
acquisition_group!(
    SENSOR_VOLTAGE_ACQ,
    SENSOR_VOLTAGE_ACQ_REG,
    "hw_sensors_voltage"
);
acquisition_group!(
    SENSOR_CURRENT_ACQ,
    SENSOR_CURRENT_ACQ_REG,
    "hw_sensors_current"
);
acquisition_group!(
    SENSOR_FAN_SPEED_ACQ,
    SENSOR_FAN_SPEED_ACQ_REG,
    "hw_sensors_fan_speed"
);
acquisition_group!(
    SENSOR_FAN_PWM_ACQ,
    SENSOR_FAN_PWM_ACQ_REG,
    "hw_sensors_fan_pwm"
);
acquisition_group!(
    SENSOR_COOLING_STATE_ACQ,
    SENSOR_COOLING_STATE_ACQ_REG,
    "hw_sensors_cooling_state"
);

#[metric(
    name = "sensor_temperature",
    description = "A temperature reading in millidegrees Celsius, from a Linux thermal zone or hwmon channel, a Corsair AXi power supply, or a macOS HID temperature service. Distinct native sources may describe the same physical component and are intentionally not deduplicated.",
    metadata = { unit = "millidegrees Celsius", acq_group = "hw_sensors_temperature" }
)]
pub static SENSOR_TEMPERATURE: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_power",
    description = "A power reading in microwatts, from a Linux hwmon channel, a Corsair AXi power supply's input power, or a macOS SMC key. INA3221 values are derived from sequential bus-voltage and current reads and carry derived=voltage_x_current metadata.",
    metadata = { unit = "microwatts", acq_group = "hw_sensors_power" }
)]
pub static SENSOR_POWER: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_voltage",
    description = "A bus-voltage reading in millivolts, from a Linux hwmon channel, a Corsair AXi power supply's AC input voltage (RMS), or a macOS SMC key. Driver-specific shunt and summation channels are excluded.",
    metadata = { unit = "millivolts", acq_group = "hw_sensors_voltage" }
)]
pub static SENSOR_VOLTAGE: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_current",
    description = "A current reading in milliamps, from a Linux hwmon channel, a Corsair AXi power supply's AC input current (RMS), or a macOS SMC key. Driver-specific summation channels are excluded.",
    metadata = { unit = "milliamps", acq_group = "hw_sensors_current" }
)]
pub static SENSOR_CURRENT: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_fan_speed",
    description = "A fan tachometer reading in revolutions per minute, from a Linux hwmon channel, a Corsair AXi power supply, or a macOS SMC key.",
    metadata = { unit = "RPM", acq_group = "hw_sensors_fan_speed" }
)]
pub static SENSOR_FAN_SPEED: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_fan_pwm",
    description = "A native hwmon fan PWM command in the driver-defined 0 to 255 range. It is not a measured fan speed.",
    metadata = { acq_group = "hw_sensors_fan_pwm" }
)]
pub static SENSOR_FAN_PWM: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_cooling_state",
    description = "A Linux thermal cooling device's current driver-defined state index. Zero does not by itself prove the hardware is not throttling.",
    metadata = { acq_group = "hw_sensors_cooling_state" }
)]
pub static SENSOR_COOLING_STATE: GaugeGroup = GaugeGroup::new(MAX_SENSORS);
