use super::discovery::{discover, Family};
use crate::agent::samplers::Sampler;

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Deserialize)]
struct Inventory {
    model: String,
    compatible: String,
    thermal_zones: Vec<Node>,
    cooling_devices: Vec<Node>,
    hwmon: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    path: String,
    resolved_path: String,
    attributes: BTreeMap<String, String>,
}

fn write(path: impl AsRef<Path>, value: impl AsRef<[u8]>) {
    let path = path.as_ref();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, value).unwrap();
}

fn materialize_inventory() -> tempfile::TempDir {
    let inventory: Inventory =
        serde_json::from_str(include_str!("fixtures/thor-sensors.json")).unwrap();
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("firmware/devicetree/base/model"),
        inventory.model,
    );
    write(
        root.path().join("firmware/devicetree/base/compatible"),
        inventory.compatible.replace('\n', "\0"),
    );
    for node in inventory
        .thermal_zones
        .iter()
        .chain(inventory.cooling_devices.iter())
        .chain(inventory.hwmon.iter())
    {
        let target = root
            .path()
            .join(node.resolved_path.trim_start_matches("/sys/"));
        std::fs::create_dir_all(&target).unwrap();
        for (name, value) in &node.attributes {
            write(target.join(name), value);
        }
        let link = root.path().join(node.path.trim_start_matches("/sys/"));
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
    }
    root
}

fn descriptors(family: Family) -> (tempfile::TempDir, Vec<super::discovery::Descriptor>) {
    let root = materialize_inventory();
    let found = discover(root.path());
    assert!(found.errors.is_empty(), "{:?}", found.errors);
    let descriptors = found
        .descriptors
        .into_iter()
        .filter(|d| d.family == family)
        .collect();
    (root, descriptors)
}

#[test]
fn thor_fixture_decodes_native_temperatures_and_exact_board_power_scope() {
    let (_root, temperatures) = descriptors(Family::Temperature);
    let values: Vec<_> = temperatures
        .iter()
        .map(|d| (d.chip.as_str(), d.channel.as_str(), d.read().unwrap()))
        .collect();
    assert!(values.contains(&("tj-thermal", "temp", 50_781)));
    assert!(values.contains(&("gpu-thermal", "temp", 50_656)));
    assert!(values.contains(&("tmp451", "temp2", 45_687)));
    assert!(values.contains(&("ina238", "temp1", 49_000)));

    let (_root, power) = descriptors(Family::Power);
    let gpu = power
        .iter()
        .find(|d| d.label.as_deref() == Some("VDD_GPU"))
        .expect("INA3221 GPU rail");
    assert!(gpu.derived);
    assert_eq!(gpu.read().unwrap(), 4_738_560);
    assert_eq!(gpu.scope, None);

    let system = power
        .iter()
        .find(|d| d.chip == "ina238")
        .expect("INA238 system power");
    assert!(!system.derived);
    assert_eq!(system.read().unwrap(), 27_422_000);
    assert_eq!(system.scope.as_deref(), Some("system"));
    assert_eq!(
        system.board_model.as_deref(),
        Some("NVIDIA Jetson AGX Thor Developer Kit")
    );
    assert_eq!(
        system.soc_compatible.as_deref(),
        Some("nvidia,p4071-0000+p3834-0008,nvidia,p3834-0008,nvidia,tegra264")
    );
}

#[test]
fn thor_fixture_uses_only_ina3221_bus_channels_and_keeps_native_rails() {
    let (_root, voltage) = descriptors(Family::Voltage);
    let ina3221: Vec<_> = voltage.iter().filter(|d| d.chip == "ina3221").collect();
    assert_eq!(
        ina3221.len(),
        3,
        "shunt and sum channels are not bus voltage"
    );
    assert_eq!(
        ina3221
            .iter()
            .map(|d| d.channel.as_str())
            .collect::<Vec<_>>(),
        ["in1", "in2", "in3"]
    );
    assert_eq!(
        ina3221
            .iter()
            .map(|d| d.label.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["VDD_GPU", "VDD_CPU_SOC_MSS", "VIN_SYS_5V0"]
    );

    let (_root, current) = descriptors(Family::Current);
    let ina3221: Vec<_> = current.iter().filter(|d| d.chip == "ina3221").collect();
    assert_eq!(ina3221.len(), 3, "summation current is not a rail");
}

fn make_hwmon(root: &Path, name: &str, attrs: &[(&str, &str)]) -> PathBuf {
    let hwmon = root.join("class/hwmon/hwmon0");
    write(hwmon.join("name"), name);
    for (attr, value) in attrs {
        write(hwmon.join(attr), value);
    }
    hwmon
}

#[test]
fn standard_channels_fan_rpm_pwm_and_cooling_state_are_read_from_files() {
    let root = tempfile::tempdir().unwrap();
    make_hwmon(
        root.path(),
        "generic",
        &[
            ("temp1_input", "42000"),
            ("temp1_label", "board"),
            ("power1_input", "9000000"),
            ("in1_input", "12000"),
            ("curr1_input", "750"),
            ("fan1_input", "1700"),
            ("pwm1", "88"),
            // pwmN_enable selects control mode; 0 is full-speed/no-control,
            // not a disabled measurement.
            ("pwm1_enable", "0"),
            ("rpm", "9999"),
        ],
    );
    let tach = root.path().join("class/hwmon/hwmon1");
    write(tach.join("name"), "pwm_tach");
    write(tach.join("rpm"), "2345");
    let cooling = root.path().join("class/thermal/cooling_device0");
    write(cooling.join("type"), "pwm-fan");
    write(cooling.join("cur_state"), "2");
    let thermal = root.path().join("class/thermal/thermal_zone0");
    write(thermal.join("type"), "board-thermal");
    write(thermal.join("temp"), "47000");
    // Thermal-zone mode controls trip-point actions, not temperature validity.
    write(thermal.join("mode"), "disabled");

    let found = discover(root.path());
    let read = |family| {
        found
            .descriptors
            .iter()
            .filter(|d| d.family == family)
            .map(|d| d.read().unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(read(Family::Temperature), [42_000, 47_000]);
    assert_eq!(read(Family::Power), [9_000_000]);
    assert_eq!(read(Family::Voltage), [12_000]);
    assert_eq!(read(Family::Current), [750]);
    assert_eq!(read(Family::FanSpeed), [1_700, 2_345]);
    assert_eq!(read(Family::FanPwm), [88]);
    assert_eq!(read(Family::CoolingState), [2]);
}

#[test]
fn disabled_faulted_missing_and_malformed_inputs_are_absent_not_zero() {
    let root = tempfile::tempdir().unwrap();
    let hwmon = make_hwmon(
        root.path(),
        "generic",
        &[
            ("temp1_input", "42000"),
            ("temp1_enable", "0"),
            ("temp2_input", "43000"),
            ("temp2_fault", "1"),
            ("temp3_input", "not-a-number"),
            ("temp4_input", "44000"),
            ("pwm1", "256"),
        ],
    );
    let found = discover(root.path());
    let mut temps: Vec<_> = found
        .descriptors
        .iter()
        .filter(|d| d.family == Family::Temperature)
        .collect();
    temps.sort_by(|a, b| a.channel.cmp(&b.channel));
    assert!(temps[0].read().is_err(), "disabled channel");
    assert!(temps[1].read().is_err(), "faulted channel");
    assert!(temps[2].read().is_err(), "malformed channel");
    std::fs::remove_file(hwmon.join("temp4_input")).unwrap();
    assert!(temps[3].read().is_err(), "removed input");
    let pwm = found
        .descriptors
        .iter()
        .find(|d| d.family == Family::FanPwm)
        .unwrap();
    assert!(pwm.read().is_err(), "PWM outside 0..=255 is malformed");
}

#[test]
fn synthetic_orin_layout_remains_generic_without_thor_scope() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("firmware/devicetree/base/model"),
        "NVIDIA Jetson AGX Orin Developer Kit",
    );
    write(
        root.path().join("firmware/devicetree/base/compatible"),
        b"nvidia,p3737-0000+p3701-0005\0nvidia,tegra234\0",
    );
    let hwmon = root
        .path()
        .join("devices/platform/bus@0/i2c@c240000/1-0040/hwmon/hwmon0");
    write(hwmon.join("name"), "ina3221");
    write(hwmon.join("in1_input"), "20000");
    write(hwmon.join("in1_label"), "VDD_IN");
    write(hwmon.join("curr1_input"), "1000");
    let link = root.path().join("class/hwmon/hwmon0");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&hwmon, link).unwrap();

    let found = discover(root.path());
    let power = found
        .descriptors
        .iter()
        .find(|d| d.family == Family::Power)
        .unwrap();
    assert_eq!(power.read().unwrap(), 20_000_000);
    assert_eq!(power.scope, None);
    assert_eq!(power.label.as_deref(), Some("VDD_IN"));
    assert_eq!(
        power.board_model.as_deref(),
        Some("NVIDIA Jetson AGX Orin Developer Kit")
    );
    assert_eq!(
        power.soc_compatible.as_deref(),
        Some("nvidia,p3737-0000+p3701-0005,nvidia,tegra234")
    );
}

#[test]
fn cached_descriptor_is_pinned_when_a_hwmon_class_symlink_is_retargeted() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("devices/platform/first/hwmon/hwmon0");
    let second = root.path().join("devices/platform/second/hwmon/hwmon1");
    write(first.join("name"), "first_chip");
    write(first.join("temp1_input"), "41000");
    write(second.join("name"), "second_chip");
    write(second.join("temp1_input"), "99000");
    let class = root.path().join("class/hwmon/hwmon0");
    std::fs::create_dir_all(class.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&first, &class).unwrap();
    let cached = family_descriptors(root.path(), Family::Temperature)
        .pop()
        .unwrap();
    assert_eq!(cached.read().unwrap(), 41_000);

    std::fs::remove_file(&class).unwrap();
    std::os::unix::fs::symlink(&second, &class).unwrap();
    assert_eq!(
        cached.read().unwrap(),
        41_000,
        "a cached identity must not follow a reused hwmon class index"
    );
}

#[test]
fn thermal_and_cooling_required_metadata_errors_are_reported() {
    let root = tempfile::tempdir().unwrap();
    let thermal = root.path().join("class/thermal/thermal_zone0");
    write(thermal.join("type"), [0xff]);
    write(thermal.join("temp"), "42000");
    let cooling = root.path().join("class/thermal/cooling_device0");
    write(cooling.join("cur_state"), "1");

    let found = discover(root.path());
    assert_eq!(found.descriptors.len(), 0);
    assert_eq!(found.errors.len(), 2, "both required type failures surface");
    assert!(found
        .errors
        .iter()
        .any(|e| e.contains("thermal_zone0/type")));
    assert!(found
        .errors
        .iter()
        .any(|e| e.contains("cooling_device0/type")));
}

fn family_descriptors(root: &Path, family: Family) -> Vec<super::discovery::Descriptor> {
    discover(root)
        .descriptors
        .into_iter()
        .filter(|d| d.family == family)
        .collect()
}

#[test]
fn lifetime_slots_are_stable_never_reused_and_bounded() {
    let root = tempfile::tempdir().unwrap();
    let hwmon = make_hwmon(
        root.path(),
        "coretemp",
        &[("temp1_input", "42000"), ("temp1_label", "Package")],
    );
    let original = family_descriptors(root.path(), Family::Temperature)
        .pop()
        .unwrap();
    let mut slots = super::Slots::new(2);
    assert_eq!(slots.reconcile(vec![original.clone()]).added, [0]);
    assert_eq!(slots.slot_of(&original.sensor), Some(0));
    assert_eq!(slots.bound(), 1);

    assert_eq!(slots.reconcile(Vec::new()).removed, [0]);
    write(hwmon.join("temp1_label"), "Package replacement");
    let replacement = family_descriptors(root.path(), Family::Temperature)
        .pop()
        .unwrap();
    assert_ne!(replacement.sensor, original.sensor);
    assert_eq!(slots.reconcile(vec![replacement.clone()]).added, [1]);
    assert_eq!(slots.slot_of(&replacement.sensor), Some(1));
    assert_eq!(slots.bound(), 2);

    // Rediscovering the identical original identity restores its old slot.
    assert!(slots.reconcile(vec![original.clone()]).added.is_empty());
    assert_eq!(slots.slot_of(&original.sensor), Some(0));
    // A third lifetime identity exceeds the cap; removed slots are not reused.
    write(hwmon.join("temp1_label"), "third identity");
    let third = family_descriptors(root.path(), Family::Temperature)
        .pop()
        .unwrap();
    assert_eq!(slots.reconcile(vec![third]).overflow, 1);
    assert_eq!(slots.bound(), 2);
}

/// Serializes tests that write process-global sensor gauge groups/windows.
static SENSOR_GLOBALS: Mutex<()> = Mutex::const_new(());

#[test]
fn families_publish_values_metadata_bounds_and_independent_windows() {
    let _globals = SENSOR_GLOBALS.blocking_lock();
    let root = tempfile::tempdir().unwrap();
    make_hwmon(
        root.path(),
        "generic",
        &[
            ("temp1_input", "42000"),
            ("temp1_label", "board"),
            ("power1_input", "9000000"),
        ],
    );
    let mut temperatures = super::Slots::new(256);
    let temp = family_descriptors(root.path(), Family::Temperature);
    let sensor = temp[0].sensor.clone();
    temperatures.reconcile(temp);
    let mut power = super::Slots::new(256);
    power.reconcile(family_descriptors(root.path(), Family::Power));

    assert_eq!(
        super::publish_family(Family::Temperature, &mut temperatures).succeeded,
        1
    );
    assert_eq!(super::metric(Family::Temperature).value(0), Some(42_000));
    assert_eq!(super::stats::SENSOR_TEMPERATURE_ACQ.member_bound(), Some(1));
    let labels = super::metric(Family::Temperature).load_metadata(0).unwrap();
    assert_eq!(labels.get("sensor"), Some(&sensor));
    assert_eq!(labels.get("source").map(String::as_str), Some("hwmon"));
    assert_eq!(labels.get("chip").map(String::as_str), Some("generic"));
    assert_eq!(labels.get("channel").map(String::as_str), Some("temp1"));
    assert_eq!(labels.get("label").map(String::as_str), Some("board"));
    let temperature_window = super::stats::SENSOR_TEMPERATURE_ACQ.window();
    assert!(temperature_window.is_some());
    assert!(super::stats::SENSOR_POWER_ACQ.window().is_none());

    assert_eq!(
        super::publish_family(Family::Power, &mut power).succeeded,
        1
    );
    assert_eq!(super::metric(Family::Power).value(0), Some(9_000_000));
    assert_eq!(
        super::stats::SENSOR_TEMPERATURE_ACQ.window(),
        temperature_window,
        "publishing power must not stamp the temperature family"
    );
    assert!(super::stats::SENSOR_POWER_ACQ.window().is_some());
}

#[test]
fn failed_and_removed_readings_clear_without_reusing_the_slot() {
    let _globals = SENSOR_GLOBALS.blocking_lock();
    let root = tempfile::tempdir().unwrap();
    let hwmon = make_hwmon(root.path(), "generic", &[("temp1_input", "42000")]);
    let descriptor = family_descriptors(root.path(), Family::Temperature)
        .pop()
        .unwrap();
    let mut slots = super::Slots::new(256);
    slots.reconcile(vec![descriptor.clone()]);
    assert_eq!(
        super::publish_family(Family::Temperature, &mut slots).succeeded,
        1
    );
    let good_window = super::stats::SENSOR_TEMPERATURE_ACQ.window();

    write(hwmon.join("temp1_input"), "invalid");
    let failed = super::publish_family(Family::Temperature, &mut slots);
    assert_eq!(failed.failed, 1);
    assert_eq!(super::metric(Family::Temperature).value(0), None);
    assert_eq!(
        super::stats::SENSOR_TEMPERATURE_ACQ.window(),
        good_window,
        "a wholly failed family read is discarded"
    );

    slots.reconcile(Vec::new());
    std::thread::sleep(Duration::from_millis(1));
    let removed = super::publish_family(Family::Temperature, &mut slots);
    assert_eq!(removed.removed, 1);
    assert_eq!(slots.bound(), 1, "lifetime bound never shrinks");
    assert_ne!(super::stats::SENSOR_TEMPERATURE_ACQ.window(), good_window);
    assert_eq!(super::metric(Family::Temperature).value(0), None);
    assert_eq!(slots.slot_of(&descriptor.sensor), Some(0));
}

#[test]
fn read_and_rediscovery_throttles_are_independent_and_include_the_first_tick() {
    let start = Instant::now();
    let mut read = super::Throttle::new();
    assert!(read.due(start, Duration::from_secs(5)));
    assert!(!read.due(start + Duration::from_secs(4), Duration::from_secs(5)));
    assert!(read.due(start + Duration::from_secs(5), Duration::from_secs(5)));

    let mut discovery = super::Throttle::new();
    assert!(discovery.due(start, Duration::from_secs(60)));
    assert!(!discovery.due(start + Duration::from_secs(59), Duration::from_secs(60)));
    assert!(discovery.due(start + Duration::from_secs(60), Duration::from_secs(60)));
}

#[test]
fn a_panicking_worker_clears_the_in_flight_latch() {
    let reading = Arc::new(AtomicBool::new(true));
    let latch = reading.clone();
    let _ = std::panic::catch_unwind(move || {
        let _in_flight = super::InFlight(latch);
        panic!("sensor sweep failed");
    });
    assert!(!reading.load(Ordering::Acquire));
}

#[tokio::test]
async fn refresh_dispatches_one_background_sweep_and_respects_both_guards() {
    let _globals = SENSOR_GLOBALS.lock().await;
    let root = tempfile::tempdir().unwrap();
    let hwmon = make_hwmon(root.path(), "generic", &[("temp1_input", "42000")]);
    let _ = super::metric(Family::Temperature).set(0, i64::MIN);
    let sampler = super::Sensors::new(Duration::from_secs(60), root.path().to_path_buf());
    assert_eq!(super::stats::SENSOR_TEMPERATURE_ACQ.member_bound(), Some(0));
    assert_eq!(super::metric(Family::Temperature).value(0), None);

    // The held latch returns before touching the cadence throttle.
    sampler.reading.store(true, Ordering::Release);
    sampler.refresh().await;
    assert!(sampler.last_read.lock().unwrap().last.is_none());
    sampler.reading.store(false, Ordering::Release);

    sampler.refresh().await;
    for _ in 0..100 {
        if !sampler.reading.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!sampler.reading.load(Ordering::Acquire));
    assert_eq!(super::metric(Family::Temperature).value(0), Some(42_000));
    assert_eq!(super::stats::SENSOR_TEMPERATURE_ACQ.member_bound(), Some(1));

    // A second refresh inside the 60-second cadence does not dispatch or read.
    write(hwmon.join("temp1_input"), "43000");
    sampler.refresh().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(super::metric(Family::Temperature).value(0), Some(42_000));
}
