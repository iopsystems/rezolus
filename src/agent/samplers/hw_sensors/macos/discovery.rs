//! Which sensors to read, from the HID services and the SMC fan count.
//!
//! The functions here do no IO: `discover` in the parent module gathers the
//! service lists and probes the SMC for each key these return.

use super::hid::HidService;
use super::smc::fourcc_str;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Family {
    Temperature,
    Power,
    Voltage,
    Current,
    FanSpeed,
}

impl Family {
    pub(crate) const ALL: [Family; 5] = [
        Family::Temperature,
        Family::Power,
        Family::Voltage,
        Family::Current,
        Family::FanSpeed,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Family::Temperature => "temperature",
            Family::Power => "power",
            Family::Voltage => "voltage",
            Family::Current => "current",
            Family::FanSpeed => "fan speed",
        }
    }
}

/// Where a candidate's value is read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    /// The temperature HID service at this index.
    Hid(usize),
    /// The SMC key named by `sensor`.
    Smc,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Candidate {
    pub family: Family,
    /// The SMC key, or the HID service's location code.
    pub sensor: String,
    pub name: String,
    pub origin: Origin,
}

/// System-wide SMC keys, by family and name. Apple does not document these
/// keys; the meanings are the ones open-source SMC tools use. On an M4 Max on
/// AC power, `VD0R` × `ID0R` matched `PDTR` within 0.5% in each of seven
/// reads from 31 to 50 W. `PSTR`, which those tools call system power, is not
/// read: on AC power each of its values was the previous `PDTR` value.
const TOTALS: [(&str, Family, &str); 3] = [
    ("PDTR", Family::Power, "DC in"),
    ("VD0R", Family::Voltage, "DC in"),
    ("ID0R", Family::Current, "DC in"),
];

/// One candidate per temperature service location, named by its `Product`.
/// The SMC publishes some sensors as two services with the same location and
/// product; the first is kept. Services with no product or a location that
/// is not a four-character code are skipped. Names are not yet unique; see
/// `disambiguate`.
pub(crate) fn temperatures(services: &[HidService]) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    services
        .iter()
        .filter(|s| !s.product.is_empty())
        .filter_map(|s| Some((s, fourcc_str(s.location)?)))
        .filter(|(s, _)| seen.insert(s.location))
        .map(|(s, sensor)| Candidate {
            family: Family::Temperature,
            sensor,
            name: s.product.clone(),
            origin: Origin::Hid(s.index),
        })
        .collect()
}

/// The rail a PMU voltage or current service measures: `PMU vbuck0` and
/// `PMU ibuck0` are both `PMU buck0`. A product without that shape, such as
/// `PMU VP0u`, is kept as it is.
fn rail_name(product: &str, letter: char) -> String {
    match product
        .strip_prefix("PMU ")
        .and_then(|r| r.strip_prefix(letter))
    {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_lowercase()) => format!("PMU {rest}"),
        _ => product.to_string(),
    }
}

/// Voltage, current and power candidates for each PMU rail the HID services
/// name, read from the SMC. A rail's voltage and current keys are the
/// services' locations (`VP0b`, `IP0b`); its power key replaces the first
/// letter with `P` (`PP0b`). A location that does not start with `V` (for a
/// voltage service) or `I` (for a current service), or that is a `TOTALS`
/// key, is skipped. Each rail is named once, from its voltage service if it
/// has one, so the three families agree.
pub(crate) fn rails(voltage: &[HidService], current: &[HidService]) -> Vec<Candidate> {
    let mut names: Vec<(String, String)> = Vec::new();
    let mut by_rail: HashMap<String, usize> = HashMap::new();
    let mut out = Vec::new();
    for (family, prefix, letter, services) in [
        (Family::Voltage, 'V', 'v', voltage),
        (Family::Current, 'I', 'i', current),
    ] {
        let mut seen = HashSet::new();
        for s in services.iter().filter(|s| !s.product.is_empty()) {
            let Some(key) = fourcc_str(s.location) else {
                continue;
            };
            if !key.starts_with(prefix)
                || TOTALS.iter().any(|(k, _, _)| *k == key)
                || !seen.insert(s.location)
            {
                continue;
            }
            let rail = key[1..].to_string();
            let i = *by_rail.entry(rail.clone()).or_insert_with(|| {
                names.push((rail.clone(), rail_name(&s.product, letter)));
                names.len() - 1
            });
            out.push(Candidate {
                family,
                sensor: key,
                name: names[i].1.clone(),
                origin: Origin::Smc,
            });
        }
    }
    out.extend(names.into_iter().map(|(rail, name)| Candidate {
        family: Family::Power,
        sensor: format!("P{rail}"),
        name,
        origin: Origin::Smc,
    }));
    out
}

/// The actual-speed key of each of `count` fans, named `fan 0`, `fan 1`, ...
/// Keys are `F<n>Ac` with a single digit, so at most ten fans are read.
pub(crate) fn fans(count: u8) -> Vec<Candidate> {
    (0..count.min(10))
        .map(|n| Candidate {
            family: Family::FanSpeed,
            sensor: format!("F{n}Ac"),
            name: format!("fan {n}"),
            origin: Origin::Smc,
        })
        .collect()
}

pub(crate) fn totals() -> Vec<Candidate> {
    TOTALS
        .iter()
        .map(|(key, family, name)| Candidate {
            family: *family,
            sensor: key.to_string(),
            name: name.to_string(),
            origin: Origin::Smc,
        })
        .collect()
}

/// Make names unique within each family: a name shared by several
/// candidates gets its sensor appended, as in `gas gauge battery (TG0B)`.
pub(crate) fn disambiguate(mut candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut count: HashMap<(usize, String), usize> = HashMap::new();
    let family_index = |f: Family| Family::ALL.iter().position(|x| *x == f).unwrap();
    for c in &candidates {
        *count
            .entry((family_index(c.family), c.name.clone()))
            .or_default() += 1;
    }
    for c in &mut candidates {
        if count[&(family_index(c.family), c.name.clone())] > 1 {
            c.name = format!("{} ({})", c.name, c.sensor);
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The services of `(page, usage)` in the M4 Max capture.
    fn captured((page, usage): (i32, i32)) -> Vec<HidService> {
        let field = |line: &str, name: &str| -> String {
            let start = line.find(&format!("{name}=")).unwrap() + name.len() + 1;
            let rest = &line[start..];
            if let Some(quoted) = rest.strip_prefix('"') {
                quoted[..quoted.find('"').unwrap()].to_string()
            } else {
                rest.split(' ').next().unwrap().to_string()
            }
        };
        include_str!("fixtures/m4max_hid.txt")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter(|l| {
                field(l, "page") == format!("0x{page:04x}")
                    && field(l, "usage") == usage.to_string()
            })
            .enumerate()
            .filter_map(|(index, l)| {
                Some(HidService {
                    index,
                    product: field(l, "product"),
                    location: field(l, "location").parse().ok()?,
                })
            })
            .collect()
    }

    fn names(candidates: &[Candidate], family: Family) -> Vec<(&str, &str)> {
        candidates
            .iter()
            .filter(|c| c.family == family)
            .map(|c| (c.sensor.as_str(), c.name.as_str()))
            .collect()
    }

    #[test]
    fn temperatures_keep_one_service_per_location() {
        let services = captured(super::super::hid::TEMPERATURE);
        assert_eq!(services.len(), 46);
        let temps = temperatures(&services);
        // 19 PMU sensors listed twice, NAND, six battery sensors; the service
        // with no product is skipped.
        assert_eq!(temps.len(), 26);
        let found = names(&temps, Family::Temperature);
        assert!(found.contains(&("TP1b", "PMU tdie1")));
        assert!(found.contains(&("TPab", "PMU tdie10")));
        assert!(found.contains(&("TN0n", "NAND CH0 temp")));
        let first_tdie1 = services.iter().find(|s| s.product == "PMU tdie1").unwrap();
        let tdie1 = temps.iter().find(|c| c.sensor == "TP1b").unwrap();
        assert_eq!(tdie1.origin, Origin::Hid(first_tdie1.index));
    }

    #[test]
    fn repeated_names_get_their_sensor() {
        let temps = disambiguate(temperatures(&captured(super::super::hid::TEMPERATURE)));
        let batteries: Vec<_> = temps
            .iter()
            .filter(|c| c.name.starts_with("gas gauge battery"))
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(batteries.len(), 6);
        assert!(batteries.contains(&"gas gauge battery (TG0B)"));
        let unique: HashSet<_> = temps.iter().map(|c| &c.name).collect();
        assert_eq!(unique.len(), temps.len());
    }

    #[test]
    fn rails_share_a_name_across_voltage_current_and_power() {
        let rails = rails(
            &captured(super::super::hid::VOLTAGE),
            &captured(super::super::hid::CURRENT),
        );
        let voltage = names(&rails, Family::Voltage);
        let current = names(&rails, Family::Current);
        let power = names(&rails, Family::Power);
        assert_eq!(voltage.len(), 20);
        assert_eq!(current.len(), 19);
        assert!(voltage.contains(&("VP0b", "PMU buck0")));
        assert!(current.contains(&("IP0b", "PMU buck0")));
        assert!(power.contains(&("PP0b", "PMU buck0")));
        assert!(voltage.contains(&("VP9l", "PMU ldo9")));
        assert!(current.contains(&("IPil", "PMU ldo18")));
        assert!(power.contains(&("PPil", "PMU ldo18")));
        // Not of the `PMU v<rail>` shape: kept as the product.
        assert!(voltage.contains(&("VP0u", "PMU VP0u")));
        // One power candidate per rail named by either family.
        assert_eq!(power.len(), 20);
    }

    #[test]
    fn rails_skip_locations_of_the_wrong_kind_and_the_totals() {
        let service = |index, product: &str, code: &[u8; 4]| HidService {
            index,
            product: product.to_string(),
            location: u32::from_be_bytes(*code),
        };
        let voltage = [
            service(0, "PMU vbuck0", b"VP0b"),
            service(1, "PMU vtemp", b"TP1b"),
            service(2, "PMU vin", b"VD0R"),
        ];
        let current = [service(0, "PMU ibuck0", b"VP0b")];
        let rails = rails(&voltage, &current);
        assert_eq!(names(&rails, Family::Voltage), vec![("VP0b", "PMU buck0")]);
        assert!(names(&rails, Family::Current).is_empty());
        assert_eq!(names(&rails, Family::Power), vec![("PP0b", "PMU buck0")]);
    }

    #[test]
    fn a_name_two_rails_share_gets_each_rails_key() {
        let service = |index, code: &[u8; 4]| HidService {
            index,
            product: "PMU vbuck0".to_string(),
            location: u32::from_be_bytes(*code),
        };
        let candidates = disambiguate(rails(&[service(0, b"VP0b"), service(1, b"VR0b")], &[]));
        assert_eq!(
            names(&candidates, Family::Voltage),
            vec![("VP0b", "PMU buck0 (VP0b)"), ("VR0b", "PMU buck0 (VR0b)")]
        );
        // A name used once in a family is unchanged, even if another family
        // uses it too.
        let mut candidates = rails(&[service(0, b"VP0b")], &[]);
        candidates.extend(totals());
        let candidates = disambiguate(candidates);
        assert_eq!(
            names(&candidates, Family::Voltage)[0],
            ("VP0b", "PMU buck0")
        );
        assert_eq!(names(&candidates, Family::Power)[0], ("PP0b", "PMU buck0"));
    }

    #[test]
    fn rail_names() {
        assert_eq!(rail_name("PMU vbuck0", 'v'), "PMU buck0");
        assert_eq!(rail_name("PMU ildo18", 'i'), "PMU ldo18");
        assert_eq!(rail_name("PMU VP0u", 'v'), "PMU VP0u");
        assert_eq!(rail_name("PMU v", 'v'), "PMU v");
        assert_eq!(rail_name("gas gauge", 'v'), "gas gauge");
    }

    #[test]
    fn fans_are_numbered_from_zero_and_capped_at_ten() {
        assert_eq!(
            names(&fans(2), Family::FanSpeed),
            vec![("F0Ac", "fan 0"), ("F1Ac", "fan 1")]
        );
        assert_eq!(fans(0).len(), 0);
        assert_eq!(fans(12).len(), 10);
    }
}
