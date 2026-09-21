//! Native Linux hardware sensor readings, kept separate by `sensor` identity.

use crate::MetricsSource;
use crate::plot::*;

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);
    let mut sensors = Group::new("Hardware Sensors", "hw_sensors");

    if has_metric(data, "sensor_temperature") {
        let temperatures = sensors.subgroup("Temperature");
        temperatures.describe(
            "Temperatures from each native sensor identity. Missing or unreadable sensors are absent.",
        );
        temperatures.plot_promql_full(
            PlotOpts::gauge("Temperature", "sensor-temperature", Unit::Count).with_axis_label("°C"),
            "sum by (sensor) (sensor_temperature) / 1000".to_string(),
        );
    }

    if has_metric(data, "sensor_power") {
        let power = sensors.subgroup("Power");
        power.describe(
            "Power per native channel, including direct readings and marked derived readings. \
             Derived power can pair sequential voltage and current reads; channels are not summed \
             across rails because rails can overlap.",
        );
        power.plot_promql_full(
            PlotOpts::gauge("Power", "sensor-power", Unit::Power).with_axis_label("Watts"),
            "sum by (sensor) (sensor_power) / 1000000".to_string(),
        );
    }

    if has_metric(data, "sensor_voltage") {
        let voltage = sensors.subgroup("Voltage");
        voltage.describe("Bus voltage per native sensor identity.");
        voltage.plot_promql_full(
            PlotOpts::gauge("Voltage", "sensor-voltage", Unit::Count).with_axis_label("Volts"),
            "sum by (sensor) (sensor_voltage) / 1000".to_string(),
        );
    }

    if has_metric(data, "sensor_current") {
        let current = sensors.subgroup("Current");
        current.describe("Current per native sensor identity.");
        current.plot_promql_full(
            PlotOpts::gauge("Current", "sensor-current", Unit::Count).with_axis_label("Amps"),
            "sum by (sensor) (sensor_current) / 1000".to_string(),
        );
    }

    if has_metric(data, "sensor_fan_speed") {
        let speed = sensors.subgroup("Fan Speed");
        speed.describe("Measured fan speed in revolutions per minute, separate from PWM demand.");
        speed.plot_promql_full(
            PlotOpts::gauge("Fan Speed", "sensor-fan-speed", Unit::Count).with_axis_label("RPM"),
            "sum by (sensor) (sensor_fan_speed)".to_string(),
        );
    }

    if has_metric(data, "sensor_fan_pwm") {
        let pwm = sensors.subgroup("Fan PWM");
        pwm.describe(
            "Fan PWM demand converted from the native 0–255 value to a percentage; it is not fan speed.",
        );
        pwm.plot_promql_full(
            PlotOpts::gauge("Fan PWM", "sensor-fan-pwm", Unit::Percentage)
                .with_axis_label("Percent")
                .percentage_range(),
            "sum by (sensor) (sensor_fan_pwm) / 255".to_string(),
        );
    }

    if has_metric(data, "sensor_cooling_state") {
        let state = sensors.subgroup("Cooling State");
        state.describe(
            "The driver-defined state index. This is an ordinal device state, not a throttling percentage.",
        );
        state.plot_promql_full(
            PlotOpts::gauge("Cooling State", "sensor-cooling-state", Unit::Count)
                .with_axis_label("State"),
            "sum by (sensor) (sensor_cooling_state)".to_string(),
        );
    }

    if !sensors.is_empty() {
        view.group(sensors);
    }

    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::{Section, View};
    use metriken_query::MemoryStore;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    const METRICS: &[&str] = &[
        "sensor_temperature",
        "sensor_power",
        "sensor_voltage",
        "sensor_current",
        "sensor_fan_speed",
        "sensor_fan_pwm",
        "sensor_cooling_state",
    ];

    fn store_with(metrics: &[&str]) -> MemoryStore {
        let store = MemoryStore::builder().sampling_interval_ms(1000).build();
        let gauges = metrics
            .iter()
            .map(|name| {
                let mut metadata = HashMap::new();
                metadata.insert("sensor".to_string(), format!("{name}:native-0"));
                metriken_exposition::Gauge::new(name.to_string(), 1, metadata)
            })
            .collect();
        store.ingest_snapshot(metriken_exposition::Snapshot::V2(
            metriken_exposition::SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                duration: Duration::ZERO,
                metadata: HashMap::new(),
                counters: vec![],
                gauges,
                histograms: vec![],
            },
        ));
        store
    }

    fn store_with_sensor_values(metric: &str, values: &[(&str, i64)]) -> MemoryStore {
        let store = MemoryStore::builder().sampling_interval_ms(1000).build();
        let gauges = values
            .iter()
            .map(|(sensor, value)| {
                let mut metadata = HashMap::new();
                metadata.insert("sensor".to_string(), (*sensor).to_string());
                metriken_exposition::Gauge::new(metric.to_string(), *value, metadata)
            })
            .collect();
        store.ingest_snapshot(metriken_exposition::Snapshot::V2(
            metriken_exposition::SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                duration: Duration::ZERO,
                metadata: HashMap::new(),
                counters: vec![],
                gauges,
                histograms: vec![],
            },
        ));
        store
    }

    fn value(view: &View) -> serde_json::Value {
        serde_json::to_value(view).unwrap()
    }

    fn plots(view: &View) -> Vec<serde_json::Value> {
        value(view)["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|group| group["subgroups"].as_array().unwrap())
            .flat_map(|subgroup| subgroup["plots"].as_array().unwrap().iter().cloned())
            .collect()
    }

    #[test]
    fn every_sensor_family_has_a_native_identity_query_and_correct_scaling() {
        let view = generate(&store_with(METRICS), Vec::<Section>::new());
        let actual: Vec<String> = plots(&view)
            .iter()
            .map(|plot| plot["promql_query"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            actual,
            [
                "sum by (sensor) (sensor_temperature) / 1000",
                "sum by (sensor) (sensor_power) / 1000000",
                "sum by (sensor) (sensor_voltage) / 1000",
                "sum by (sensor) (sensor_current) / 1000",
                "sum by (sensor) (sensor_fan_speed)",
                "sum by (sensor) (sensor_fan_pwm) / 255",
                "sum by (sensor) (sensor_cooling_state)",
            ]
        );
    }

    #[test]
    fn generated_temperature_query_evaluates_to_scaled_native_series() {
        let store = store_with_sensor_values(
            "sensor_temperature",
            &[
                ("ambient:below-freezing", -5_000),
                ("thermal_zone:CPU", 42_500),
                ("hwmon:GPU", 55_250),
            ],
        );
        let view = generate(&store, vec![]);
        let query = plots(&view)[0]["promql_query"]
            .as_str()
            .unwrap()
            .to_string();
        let result = store.query(&query, Some(1_700_000_000.0)).unwrap();
        let mut series: Vec<(String, f64)> = match result {
            metriken_query::QueryResult::Vector { result } => result
                .into_iter()
                .map(|sample| (sample.metric["sensor"].clone(), sample.value.1))
                .collect(),
            other => panic!("expected vector, got {other:?}"),
        };
        series.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            series,
            [
                ("ambient:below-freezing".to_string(), -5.0),
                ("hwmon:GPU".to_string(), 55.25),
                ("thermal_zone:CPU".to_string(), 42.5),
            ]
        );
    }

    #[test]
    fn plots_use_physical_units_and_only_pwm_is_a_percentage() {
        let view = generate(&store_with(METRICS), vec![]);
        let plots = plots(&view);
        let axis_labels: Vec<Option<&str>> = plots
            .iter()
            .map(|plot| plot["opts"]["format"]["y_axis_label"].as_str())
            .collect();
        assert_eq!(
            axis_labels,
            [
                Some("°C"),
                Some("Watts"),
                Some("Volts"),
                Some("Amps"),
                Some("RPM"),
                Some("Percent"),
                Some("State"),
            ]
        );
        assert_eq!(plots[5]["opts"]["format"]["unit_system"], "percentage");
        assert_eq!(plots[5]["opts"]["format"]["range"]["min"], 0.0);
        assert_eq!(plots[5]["opts"]["format"]["range"]["max"], 1.0);
        assert_eq!(plots[6]["opts"]["format"]["unit_system"], "count");
        assert!(plots[6]["opts"]["format"]["range"].is_null());
    }

    #[test]
    fn each_chart_is_conditional_and_absent_sampler_is_empty() {
        for metric in METRICS {
            let view = generate(&store_with(&[metric]), vec![]);
            let plots = plots(&view);
            assert_eq!(plots.len(), 1, "expected one plot for {metric}");
            assert_eq!(
                plots[0]["promql_query"]
                    .as_str()
                    .unwrap()
                    .matches("sensor_")
                    .count(),
                1
            );
        }

        let empty = generate(&MemoryStore::builder().build(), vec![]);
        assert!(value(&empty)["groups"].as_array().unwrap().is_empty());
    }

    #[test]
    fn descriptions_explain_non_additive_and_driver_defined_values() {
        let view = generate(&store_with(METRICS), vec![]);
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.contains("not summed across rails"));
        assert!(json.contains("0–255"));
        assert!(json.contains("driver-defined state index"));
        assert!(!json.contains("total power"));
    }
}
