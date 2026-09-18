//! Linux thermal, hwmon and cooling-device discovery.
//!
//! Discovery reads only identity and capability files. Values stay behind
//! [`Descriptor::read`] so callers can cache descriptors between discovery
//! passes without coupling sysfs decoding to metric publication.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Family {
    Temperature,
    Power,
    Voltage,
    Current,
    FanSpeed,
    FanPwm,
    CoolingState,
}

impl Family {
    pub const ALL: [Self; 7] = [
        Self::Temperature,
        Self::Power,
        Self::Voltage,
        Self::Current,
        Self::FanSpeed,
        Self::FanPwm,
        Self::CoolingState,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Temperature => "temperature",
            Self::Power => "power",
            Self::Voltage => "voltage",
            Self::Current => "current",
            Self::FanSpeed => "fan_speed",
            Self::FanPwm => "fan_pwm",
            Self::CoolingState => "cooling_state",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Descriptor {
    pub family: Family,
    /// Stable, family-local identity. It contains no discovery-root prefix.
    pub sensor: String,
    pub source: String,
    pub chip: String,
    pub channel: String,
    pub label: Option<String>,
    pub scope: Option<String>,
    pub board_model: Option<String>,
    pub soc_compatible: Option<String>,
    pub derived: bool,
    pub(crate) input: Input,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Input {
    Direct(ReadSpec),
    Product(ReadSpec, ReadSpec),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReadSpec {
    value: PathBuf,
    enabled: Option<Control>,
    fault: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Control {
    Nonzero(PathBuf),
}

#[derive(Debug, Default)]
pub struct Discovery {
    pub descriptors: Vec<Descriptor>,
    /// Metadata/discovery failures. Missing classes are normal and omitted.
    pub errors: Vec<String>,
}

impl ReadSpec {
    fn read(&self) -> io::Result<i64> {
        if let Some(enabled) = &self.enabled {
            let enabled = match enabled {
                Control::Nonzero(path) => parse(path)? != 0,
            };
            if !enabled {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "sensor channel is disabled",
                ));
            }
        }
        if let Some(path) = &self.fault {
            if parse(path)? != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "sensor channel reports a fault",
                ));
            }
        }
        parse(&self.value)
    }
}

impl Descriptor {
    pub fn read(&self) -> io::Result<i64> {
        let value =
            match &self.input {
                Input::Direct(input) => input.read(),
                // hwmon exposes INA3221 voltage in mV and current in mA; their
                // product is µW. The two sequential reads are not atomic.
                Input::Product(voltage, current) => voltage
                    .read()?
                    .checked_mul(current.read()?)
                    .ok_or(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "derived power overflow",
                    )),
            }?;
        if self.family == Family::FanPwm && !(0..=255).contains(&value) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fan PWM is outside the hwmon 0..=255 range",
            ));
        }
        Ok(value)
    }
}

pub fn discover(sysfs: &Path) -> Discovery {
    let mut found = Discovery::default();
    let board = Board::read(sysfs);
    discover_thermal(sysfs, &mut found);
    discover_cooling(sysfs, &mut found);
    discover_hwmon(sysfs, &board, &mut found);
    for descriptor in &mut found.descriptors {
        descriptor.board_model.clone_from(&board.model);
        descriptor.soc_compatible.clone_from(&board.compatible);
        // These are recorded series context. Include them in the published
        // identity too, so a metadata replacement can never relabel a slot.
        if let Some(model) = &board.model {
            descriptor.sensor.push_str(":board_model=");
            descriptor.sensor.push_str(model);
        }
        if let Some(compatible) = &board.compatible {
            descriptor.sensor.push_str(":soc_compatible=");
            descriptor.sensor.push_str(compatible);
        }
    }
    found
        .descriptors
        .sort_by(|a, b| (a.family, a.sensor.as_str()).cmp(&(b.family, b.sensor.as_str())));
    found
}

#[derive(Default)]
struct Board {
    model: Option<String>,
    compatible: Option<String>,
    verified_thor: bool,
}

impl Board {
    fn read(sysfs: &Path) -> Self {
        let base = sysfs.join("firmware/devicetree/base");
        let model = read_text(&base.join("model")).ok();
        let compatible = std::fs::read(base.join("compatible")).ok();
        let compatible: Vec<_> = compatible
            .as_deref()
            .unwrap_or_default()
            .split(|b| *b == 0 || *b == b'\n')
            .filter(|v| !v.is_empty())
            .filter_map(|v| std::str::from_utf8(v).ok())
            .collect();
        let verified_thor = model.as_deref() == Some("NVIDIA Jetson AGX Thor Developer Kit")
            && compatible
                == [
                    "nvidia,p4071-0000+p3834-0008",
                    "nvidia,p3834-0008",
                    "nvidia,tegra264",
                ];
        Self {
            model,
            compatible: (!compatible.is_empty()).then(|| compatible.join(",")),
            verified_thor,
        }
    }
}

fn discover_thermal(sysfs: &Path, found: &mut Discovery) {
    for node in class_entries(sysfs, "class/thermal", "thermal_zone", &mut found.errors) {
        // Pin cached value paths to the resolved device. Class indices can be
        // removed and reused before the next discovery pass.
        let node = std::fs::canonicalize(&node).unwrap_or(node);
        let chip = match read_text(&node.join("type")) {
            Ok(chip) => chip,
            Err(e) => {
                found
                    .errors
                    .push(format!("{}: {e}", node.join("type").display()));
                continue;
            }
        };
        let value = node.join("temp");
        if !value.exists() {
            continue;
        }
        let input = ReadSpec {
            value,
            // `mode` enables kernel trip-point actions; it does not disable
            // the thermal-zone temperature measurement.
            enabled: None,
            fault: None,
        };
        add(
            found,
            sysfs,
            &node,
            Family::Temperature,
            "thermal",
            &chip,
            "temp",
            None,
            None,
            false,
            Input::Direct(input),
        );
    }
}

fn discover_cooling(sysfs: &Path, found: &mut Discovery) {
    for node in class_entries(sysfs, "class/thermal", "cooling_device", &mut found.errors) {
        let node = std::fs::canonicalize(&node).unwrap_or(node);
        let chip = match read_text(&node.join("type")) {
            Ok(chip) => chip,
            Err(e) => {
                found
                    .errors
                    .push(format!("{}: {e}", node.join("type").display()));
                continue;
            }
        };
        let value = node.join("cur_state");
        if !value.exists() {
            continue;
        }
        add(
            found,
            sysfs,
            &node,
            Family::CoolingState,
            "cooling_device",
            &chip,
            "cur_state",
            None,
            None,
            false,
            Input::Direct(ReadSpec {
                value,
                enabled: None,
                fault: None,
            }),
        );
    }
}

fn discover_hwmon(sysfs: &Path, board: &Board, found: &mut Discovery) {
    for node in class_entries(sysfs, "class/hwmon", "hwmon", &mut found.errors) {
        let node = std::fs::canonicalize(&node).unwrap_or(node);
        let chip = match read_text(&node.join("name")) {
            Ok(chip) => chip,
            Err(e) => {
                found.errors.push(format!("{}: {e}", node.display()));
                continue;
            }
        };
        let device = device_identity(sysfs, &node, true);
        let names = attribute_names(&node, &mut found.errors);

        // INA3221's in1..3 are bus voltages. in4..7 are shunt/sum channels;
        // curr4 is a summation channel. Publish only the three physical rails.
        if chip == "ina3221" {
            for n in 1..=3 {
                let voltage_channel = format!("in{n}");
                let current_channel = format!("curr{n}");
                let voltage = direct_spec(&node, &voltage_channel, "_input");
                let current = direct_spec(&node, &current_channel, "_input");
                let label = channel_label(&node, &voltage_channel);
                if disconnected(label.as_deref()) {
                    continue;
                }
                if let Some(spec) = voltage.clone() {
                    add_with_device(
                        found,
                        &device,
                        Family::Voltage,
                        "hwmon",
                        &chip,
                        &voltage_channel,
                        label.clone(),
                        None,
                        false,
                        Input::Direct(spec),
                    );
                }
                if let Some(spec) = current.clone() {
                    add_with_device(
                        found,
                        &device,
                        Family::Current,
                        "hwmon",
                        &chip,
                        &current_channel,
                        label.clone(),
                        None,
                        false,
                        Input::Direct(spec),
                    );
                }
                let direct_power = direct_spec(&node, &format!("power{n}"), "_input");
                let (input, derived) = match (direct_power, voltage, current) {
                    (Some(power), _, _) => (Some(Input::Direct(power)), false),
                    (None, Some(voltage), Some(current)) => {
                        (Some(Input::Product(voltage, current)), true)
                    }
                    _ => (None, false),
                };
                if let Some(input) = input {
                    add_with_device(
                        found,
                        &device,
                        Family::Power,
                        "hwmon",
                        &chip,
                        &format!("power{n}"),
                        label,
                        None,
                        derived,
                        input,
                    );
                }
            }
        }

        for name in names {
            let Some((prefix, index)) = input_attribute(&name) else {
                continue;
            };
            let channel = format!("{prefix}{index}");
            let label = channel_label(&node, &channel);
            if disconnected(label.as_deref()) {
                continue;
            }
            let family = match prefix {
                "temp" => Family::Temperature,
                "power" => Family::Power,
                "in" => Family::Voltage,
                "curr" => Family::Current,
                "fan" => Family::FanSpeed,
                _ => continue,
            };
            if chip == "ina3221" && matches!(prefix, "power" | "in" | "curr") {
                continue;
            }
            // INA238 in0 is shunt voltage; in1 is its bus voltage.
            if chip == "ina238" && prefix == "in" && index != 1 {
                continue;
            }
            let scope = (family == Family::Power
                && chip == "ina238"
                && board.verified_thor
                && device == "/devices/platform/bus@0/c600000.i2c/i2c-2/2-0044")
                .then(|| "system".to_string());
            if let Some(input) = direct_spec(&node, &channel, "_input") {
                add_with_device(
                    found,
                    &device,
                    family,
                    "hwmon",
                    &chip,
                    &channel,
                    label,
                    scope,
                    false,
                    Input::Direct(input),
                );
            }
        }

        for name in attribute_names(&node, &mut found.errors) {
            if let Some(index) = exact_index(&name, "pwm") {
                let channel = format!("pwm{index}");
                if let Some(input) = direct_spec(&node, &channel, "") {
                    // pwmN_enable selects the fan control method (0 means no
                    // control/full speed); it is not a sensor enable bit.
                    let input = ReadSpec {
                        value: input.value,
                        enabled: None,
                        fault: input.fault,
                    };
                    add_with_device(
                        found,
                        &device,
                        Family::FanPwm,
                        "hwmon",
                        &chip,
                        &channel,
                        channel_label(&node, &channel),
                        None,
                        false,
                        Input::Direct(input),
                    );
                }
            }
        }
        if chip == "pwm_tach" && node.join("rpm").exists() {
            add_with_device(
                found,
                &device,
                Family::FanSpeed,
                "hwmon",
                &chip,
                "rpm",
                channel_label(&node, "rpm"),
                None,
                false,
                Input::Direct(ReadSpec {
                    value: node.join("rpm"),
                    enabled: control(&node, "rpm"),
                    fault: fault(&node, "rpm"),
                }),
            );
        }
    }
}

fn class_entries(
    sysfs: &Path,
    class: &str,
    prefix: &str,
    errors: &mut Vec<String>,
) -> Vec<PathBuf> {
    let path = sysfs.join(class);
    let entries = match std::fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            errors.push(format!("{}: {e}", path.display()));
            return Vec::new();
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry)
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(prefix)) =>
            {
                paths.push(entry.path())
            }
            Ok(_) => {}
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    paths.sort();
    paths
}

fn attribute_names(node: &Path, errors: &mut Vec<String>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    match std::fs::read_dir(node) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) => {
                        if let Some(name) = entry.file_name().to_str() {
                            names.insert(name.to_string());
                        }
                    }
                    Err(e) => errors.push(format!("{}: {e}", node.display())),
                }
            }
        }
        Err(e) => errors.push(format!("{}: {e}", node.display())),
    }
    names
}

fn input_attribute(name: &str) -> Option<(&str, usize)> {
    let stem = name.strip_suffix("_input")?;
    for prefix in ["temp", "power", "in", "curr", "fan"] {
        if let Some(index) = exact_index(stem, prefix) {
            return Some((prefix, index));
        }
    }
    None
}

fn exact_index(name: &str, prefix: &str) -> Option<usize> {
    let digits = name.strip_prefix(prefix)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn direct_spec(node: &Path, channel: &str, suffix: &str) -> Option<ReadSpec> {
    let value = node.join(format!("{channel}{suffix}"));
    value.exists().then(|| ReadSpec {
        value,
        enabled: control(node, channel),
        fault: fault(node, channel),
    })
}

fn control(node: &Path, channel: &str) -> Option<Control> {
    let path = node.join(format!("{channel}_enable"));
    path.exists().then_some(Control::Nonzero(path))
}

fn fault(node: &Path, channel: &str) -> Option<PathBuf> {
    let path = node.join(format!("{channel}_fault"));
    path.exists().then_some(path)
}

fn channel_label(node: &Path, channel: &str) -> Option<String> {
    read_text(&node.join(format!("{channel}_label"))).ok()
}

fn disconnected(label: Option<&str>) -> bool {
    label.is_some_and(|label| {
        let label = label.trim().to_ascii_lowercase();
        matches!(
            label.as_str(),
            "nc" | "n/c" | "not connected" | "unconnected"
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn add(
    found: &mut Discovery,
    sysfs: &Path,
    node: &Path,
    family: Family,
    source: &str,
    chip: &str,
    channel: &str,
    label: Option<String>,
    scope: Option<String>,
    derived: bool,
    input: Input,
) {
    let device = device_identity(sysfs, node, false);
    add_with_device(
        found, &device, family, source, chip, channel, label, scope, derived, input,
    );
}

#[allow(clippy::too_many_arguments)]
fn add_with_device(
    found: &mut Discovery,
    device: &str,
    family: Family,
    source: &str,
    chip: &str,
    channel: &str,
    label: Option<String>,
    scope: Option<String>,
    derived: bool,
    input: Input,
) {
    let mut sensor = format!("{source}:{device}:{chip}:{channel}");
    if let Some(label) = &label {
        sensor.push_str(":label=");
        sensor.push_str(label);
    }
    if let Some(scope) = &scope {
        sensor.push_str(":scope=");
        sensor.push_str(scope);
    }
    if derived {
        sensor.push_str(":derived=voltage_x_current");
    }
    found.descriptors.push(Descriptor {
        family,
        sensor,
        source: source.to_string(),
        chip: chip.to_string(),
        channel: channel.to_string(),
        label,
        scope,
        board_model: None,
        soc_compatible: None,
        derived,
        input,
    });
}

fn device_identity(sysfs: &Path, node: &Path, hwmon: bool) -> String {
    let canonical = std::fs::canonicalize(node).unwrap_or_else(|_| node.to_path_buf());
    let in_class = canonical.starts_with(sysfs.join("class"));
    let mut device = canonical.as_path();
    if hwmon && !in_class {
        if device
            .file_name()
            .and_then(|v| v.to_str())
            .is_some_and(|name| name.starts_with("hwmon"))
        {
            device = device.parent().unwrap_or(device);
            if device.file_name().and_then(|v| v.to_str()) == Some("hwmon") {
                device = device.parent().unwrap_or(device);
            }
        }
    }
    let relative = device.strip_prefix(sysfs).unwrap_or(device);
    format!("/{}", relative.to_string_lossy().trim_start_matches('/'))
}

fn read_text(path: &Path) -> io::Result<String> {
    let value = std::fs::read(path)?;
    let value =
        std::str::from_utf8(&value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(value
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string())
}

fn parse(path: &Path) -> io::Result<i64> {
    read_text(path)?.parse().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {e}", path.display()),
        )
    })
}
