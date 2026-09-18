use metriken::*;

use crate::agent::timing::AcquisitionGroup;
use linkme::distributed_slice;

/// Per-family lifetime identity cap. Slots are never reused in one process.
pub const MAX_SENSORS: usize = 256;

macro_rules! acquisition_group {
    ($static:ident, $registration:ident, $name:literal) => {
        pub static $static: AcquisitionGroup =
            AcquisitionGroup::new(crate::agent::samplers::bpf_sampler_name("sensors"), $name);
        #[distributed_slice(crate::agent::samplers::ACQUISITION_GROUPS)]
        static $registration: &'static AcquisitionGroup = &$static;
    };
}

acquisition_group!(
    SENSOR_TEMPERATURE_ACQ,
    SENSOR_TEMPERATURE_ACQ_REG,
    "sensors_temperature"
);
acquisition_group!(SENSOR_POWER_ACQ, SENSOR_POWER_ACQ_REG, "sensors_power");
acquisition_group!(
    SENSOR_VOLTAGE_ACQ,
    SENSOR_VOLTAGE_ACQ_REG,
    "sensors_voltage"
);
acquisition_group!(
    SENSOR_CURRENT_ACQ,
    SENSOR_CURRENT_ACQ_REG,
    "sensors_current"
);
acquisition_group!(
    SENSOR_FAN_SPEED_ACQ,
    SENSOR_FAN_SPEED_ACQ_REG,
    "sensors_fan_speed"
);
acquisition_group!(
    SENSOR_FAN_PWM_ACQ,
    SENSOR_FAN_PWM_ACQ_REG,
    "sensors_fan_pwm"
);
acquisition_group!(
    SENSOR_COOLING_STATE_ACQ,
    SENSOR_COOLING_STATE_ACQ_REG,
    "sensors_cooling_state"
);

#[metric(
    name = "sensor_temperature",
    description = "A native Linux thermal-zone or hwmon temperature reading in millidegrees Celsius. Distinct native sources may describe the same physical component and are intentionally not deduplicated.",
    metadata = { unit = "millidegrees Celsius", acq_group = "sensors_temperature" }
)]
pub static SENSOR_TEMPERATURE: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_power",
    description = "A native hwmon power reading in microwatts. INA3221 values are derived from sequential bus-voltage and current reads and carry derived=voltage_x_current metadata.",
    metadata = { unit = "microwatts", acq_group = "sensors_power" }
)]
pub static SENSOR_POWER: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_voltage",
    description = "A native hwmon bus-voltage reading in millivolts. Driver-specific shunt and summation channels are excluded.",
    metadata = { unit = "millivolts", acq_group = "sensors_voltage" }
)]
pub static SENSOR_VOLTAGE: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_current",
    description = "A native hwmon current reading in milliamps. Driver-specific summation channels are excluded.",
    metadata = { unit = "milliamps", acq_group = "sensors_current" }
)]
pub static SENSOR_CURRENT: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_fan_speed",
    description = "A native hwmon fan tachometer reading in revolutions per minute.",
    metadata = { unit = "RPM", acq_group = "sensors_fan_speed" }
)]
pub static SENSOR_FAN_SPEED: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_fan_pwm",
    description = "A native hwmon fan PWM command in the driver-defined 0 to 255 range. It is not a measured fan speed.",
    metadata = { acq_group = "sensors_fan_pwm" }
)]
pub static SENSOR_FAN_PWM: GaugeGroup = GaugeGroup::new(MAX_SENSORS);

#[metric(
    name = "sensor_cooling_state",
    description = "A Linux thermal cooling device's current driver-defined state index. Zero does not by itself prove the hardware is not throttling.",
    metadata = { acq_group = "sensors_cooling_state" }
)]
pub static SENSOR_COOLING_STATE: GaugeGroup = GaugeGroup::new(MAX_SENSORS);
