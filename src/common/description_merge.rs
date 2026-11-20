//! One description per metric name, from the descriptions of every series
//! that has the name.
//!
//! Series of one name can describe different things: one `phase` of a
//! latency, one `reason` for a flusher pass, one temperature sensor. The
//! descriptions map (`/metrics/descriptions`, a recording's `descriptions`
//! metadata, the build-time table) is keyed by name, so when the series
//! disagree the name's entry lists each label value with its own text.
//!
//! `build.rs` includes this file with `#[path]`, so it uses only `std`.

use std::collections::{BTreeMap, BTreeSet};

/// One series of a name: its static metadata and its description.
pub type Series = (BTreeMap<String, String>, String);

/// Metadata keys that never tell a reader which series a description is
/// for: the exposition removes them, so no series carries them as a label.
const IGNORED_KEYS: &[&str] = &["acq_group"];

/// The description for a name whose series carry `series`, each a series'
/// static metadata and description. `None` when there are none.
///
/// When every series has the same description, that description. Otherwise
/// the metadata keys whose values differ between the series name each
/// series, and each distinct description is listed once after the label
/// values it applies to, ordered by those label values:
///
/// `One series per phase. phase=flushing: ... phase=wait: ...`
///
/// Series with the same label values and different descriptions (AMD's SMI
/// and sysfs series, which differ only in `acq_group`) list each one, joined
/// by `Or:`.
pub fn merge(series: &[Series]) -> Option<String> {
    let descriptions: BTreeSet<&str> = series.iter().map(|(_, d)| d.as_str()).collect();
    match descriptions.len() {
        0 => return None,
        1 => return descriptions.into_iter().next().map(str::to_string),
        _ => {}
    }

    let keys = distinguishing_keys(series);
    let by_selector = descriptions_by_selector(series, &keys);

    // Selectors whose descriptions are the same are listed together.
    let mut by_text: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (selector, descriptions) in by_selector {
        let text = descriptions
            .into_iter()
            .map(sentence)
            .collect::<Vec<_>>()
            .join(" Or: ");
        by_text.entry(text).or_default().insert(selector);
    }
    let mut items: Vec<(String, String)> = by_text
        .into_iter()
        .map(|(text, selectors)| (selectors.into_iter().collect::<Vec<_>>().join("; "), text))
        .collect();
    items.sort();

    let mut out = if keys.is_empty() {
        "Series of this name are described differently.".to_string()
    } else {
        format!("One series per {}.", list(&keys))
    };
    for (selector, text) in items {
        out.push(' ');
        out.push_str(&selector);
        out.push_str(": ");
        out.push_str(&text);
    }
    Some(out)
}

/// Metadata keys, other than [`IGNORED_KEYS`] and `__` keys, whose value (or
/// absence) is not the same on every series.
fn distinguishing_keys(series: &[Series]) -> Vec<&str> {
    let all_keys: BTreeSet<&str> = series
        .iter()
        .flat_map(|(labels, _)| labels.keys().map(String::as_str))
        .filter(|k| !k.starts_with("__") && !IGNORED_KEYS.contains(k))
        .collect();
    all_keys
        .into_iter()
        .filter(|k| {
            let values: BTreeSet<Option<&String>> =
                series.iter().map(|(labels, _)| labels.get(*k)).collect();
            values.len() > 1
        })
        .collect()
}

/// Each series' label values for `keys` (`k=v, k2=v2`, or `no k` for a
/// series that has none of them), with the descriptions of the series that
/// carry them.
fn descriptions_by_selector<'a>(
    series: &'a [Series],
    keys: &[&str],
) -> BTreeMap<String, BTreeSet<&'a str>> {
    let mut out: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for (labels, description) in series {
        let selector: Vec<String> = keys
            .iter()
            .filter_map(|k| labels.get(*k).map(|v| format!("{k}={v}")))
            .collect();
        let selector = match (selector.is_empty(), keys.is_empty()) {
            (false, _) => selector.join(", "),
            (true, false) => format!("no {}", keys.join(" or ")),
            (true, true) => "every series".to_string(),
        };
        out.entry(selector)
            .or_default()
            .insert(description.as_str());
    }
    out
}

/// `a`, `a and b`, `a, b and c`.
fn list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `description` ending in a full stop.
fn sentence(description: &str) -> String {
    let mut text = description.trim_end().to_string();
    if !text.ends_with(['.', '!', '?']) {
        text.push('.');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(labels: &[(&str, &str)], description: &str) -> Series {
        (
            labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            description.to_string(),
        )
    }

    #[test]
    fn one_description_is_kept_as_is() {
        let series = [
            s(&[("op", "read")], "Bytes moved"),
            s(&[("op", "write")], "Bytes moved"),
        ];
        assert_eq!(merge(&series).as_deref(), Some("Bytes moved"));
        assert_eq!(merge(&[]), None);
    }

    #[test]
    fn differing_descriptions_are_listed_by_the_label_that_differs() {
        let series = [
            s(
                &[
                    ("unit", "nanoseconds"),
                    ("phase", "wait"),
                    ("acq_group", "g"),
                ],
                "Time a handle waited",
            ),
            s(
                &[
                    ("unit", "nanoseconds"),
                    ("phase", "flushing"),
                    ("acq_group", "g"),
                ],
                "Time spent flushing.",
            ),
        ];
        assert_eq!(
            merge(&series).as_deref(),
            Some(
                "One series per phase. phase=flushing: Time spent flushing. \
                 phase=wait: Time a handle waited."
            )
        );
    }

    #[test]
    fn label_values_sharing_a_description_are_listed_together() {
        let series = [
            s(&[("reason", "a")], "Shared"),
            s(&[("reason", "c")], "Other"),
            s(&[("reason", "b")], "Shared"),
        ];
        assert_eq!(
            merge(&series).as_deref(),
            Some("One series per reason. reason=a; reason=b: Shared. reason=c: Other.")
        );
    }

    #[test]
    fn a_series_without_the_key_is_named_by_its_absence() {
        let series = [s(&[("sensor", "edge")], "Edge"), s(&[], "The GPU")];
        assert_eq!(
            merge(&series).as_deref(),
            Some("One series per sensor. no sensor: The GPU. sensor=edge: Edge.")
        );
    }

    #[test]
    fn descriptions_with_the_same_label_values_are_joined() {
        // AMD's SMI and sysfs series differ only in their acquisition group.
        let series = [
            s(&[("vendor", "amd"), ("acq_group", "smi")], "Socket power"),
            s(&[("vendor", "amd"), ("acq_group", "sysfs")], "Board power"),
            s(&[("vendor", "nvidia"), ("acq_group", "nv")], "Board power"),
        ];
        assert_eq!(
            merge(&series).as_deref(),
            Some(
                "One series per vendor. vendor=amd: Board power. Or: Socket power. \
                 vendor=nvidia: Board power."
            )
        );
    }

    #[test]
    fn series_with_the_same_metadata_list_each_description() {
        let series = [s(&[("acq_group", "g")], "A"), s(&[("acq_group", "g")], "B")];
        assert_eq!(
            merge(&series).as_deref(),
            Some("Series of this name are described differently. every series: A. Or: B.")
        );
    }

    #[test]
    fn three_keys_are_listed_with_commas() {
        let series = [
            s(&[("a", "1"), ("b", "1"), ("c", "1")], "X"),
            s(&[("a", "2"), ("b", "2"), ("c", "2")], "Y"),
        ];
        assert!(merge(&series)
            .unwrap()
            .starts_with("One series per a, b and c. "));
    }

    #[test]
    fn the_result_does_not_depend_on_registry_order() {
        let mut series = vec![
            s(&[("phase", "a")], "A"),
            s(&[("phase", "b")], "B"),
            s(&[("phase", "c")], "C"),
        ];
        let forward = merge(&series);
        series.reverse();
        assert_eq!(merge(&series), forward);
    }
}
