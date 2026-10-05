//! A recording's metadata after the agent restarts mid-recording.
//!
//! A recording's metadata is read from the agent when the recording opens:
//! its version, producer epoch, systeminfo and metric descriptions. When the
//! agent restarts, every cumulative counter starts again from zero and the new
//! process may be a different build. [`metadata_patch`] is what changes: the
//! epoch history, and the agent's own description of itself.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::parquet_metadata::{
    KEY_DESCRIPTIONS, KEY_PRODUCER_EPOCH, KEY_PRODUCER_EPOCHS, KEY_SYSTEMINFO, KEY_VERSION,
};

/// A restart seen on an endpoint, waiting for its first row and then for the
/// new agent's metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restart {
    /// The new process's producer epoch.
    pub epoch: String,
    /// The timestamp of the first row staged from the new process; `None`
    /// until one is.
    pub from_ts: Option<u64>,
}

/// What the restarted agent says about itself.
#[derive(Clone, Debug, Default)]
pub struct Agent {
    pub version: Option<String>,
    pub systeminfo: Option<String>,
    pub descriptions: Option<String>,
}

/// The keys to patch into a recording's metadata, `current`, for the restarts
/// in `restarts` (oldest first, each with its `from_ts`), with `agent` the
/// newest process's description. `first_ts` is the recording's first row, the
/// start of the epoch `current` was opened with. A key in `user_keys` was set
/// by the user and is left alone.
///
/// - `producer_epoch` becomes the newest epoch.
/// - `producer_epochs` gains one entry per restart. The first restart seeds it
///   with the epoch the recording opened with, from `first_ts`.
/// - `version` and `systeminfo` become the new agent's, when it reported them.
/// - `descriptions` becomes the union of the old and new maps, the new
///   description winning for a metric in both, so the rows from before the
///   restart keep their descriptions.
pub fn metadata_patch(
    current: &BTreeMap<String, String>,
    first_ts: u64,
    restarts: &[Restart],
    agent: &Agent,
    user_keys: &dyn Fn(&str) -> bool,
) -> BTreeMap<String, String> {
    let mut patch = BTreeMap::new();
    let Some(newest) = restarts.last() else {
        return patch;
    };

    let mut epochs: Vec<Value> = current
        .get(KEY_PRODUCER_EPOCHS)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    if epochs.is_empty() {
        if let Some(epoch) = current.get(KEY_PRODUCER_EPOCH) {
            epochs.push(entry(epoch, first_ts, current.get(KEY_VERSION)));
        }
    }
    for (i, restart) in restarts.iter().enumerate() {
        let last = epochs
            .last()
            .and_then(|e| e.get("epoch"))
            .and_then(Value::as_str);
        if last == Some(restart.epoch.as_str()) {
            continue;
        }
        // Only the newest process could be asked for its version; an epoch
        // that came and went between two fetches has none on record.
        let version = (i + 1 == restarts.len())
            .then_some(agent.version.as_ref())
            .flatten();
        epochs.push(entry(
            &restart.epoch,
            restart.from_ts.unwrap_or(first_ts),
            version,
        ));
    }
    patch.insert(
        KEY_PRODUCER_EPOCHS.to_string(),
        Value::Array(epochs).to_string(),
    );
    patch.insert(KEY_PRODUCER_EPOCH.to_string(), newest.epoch.clone());

    let mut set = |key: &str, value: Option<String>| {
        if let Some(value) = value {
            if !user_keys(key) {
                patch.insert(key.to_string(), value);
            }
        }
    };
    set(KEY_VERSION, agent.version.clone());
    set(KEY_SYSTEMINFO, agent.systeminfo.clone());
    set(
        KEY_DESCRIPTIONS,
        merge_descriptions(current.get(KEY_DESCRIPTIONS), agent.descriptions.as_ref()),
    );
    patch
}

fn entry(epoch: &str, from_ts: u64, version: Option<&String>) -> Value {
    let mut e = json!({ "epoch": epoch, "from_ts": from_ts });
    if let Some(version) = version {
        e["version"] = Value::String(version.clone());
    }
    e
}

/// The union of two JSON description maps, `new` winning on a shared key.
/// When either does not parse as a JSON object, `new` replaces `old`.
fn merge_descriptions(old: Option<&String>, new: Option<&String>) -> Option<String> {
    let new = new?;
    let parse = |s: &String| serde_json::from_str::<Map<String, Value>>(s).ok();
    match (old.and_then(parse), parse(new)) {
        (Some(mut merged), Some(new_map)) => {
            merged.extend(new_map);
            Some(Value::Object(merged).to_string())
        }
        _ => Some(new.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(epoch: &str, version: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            (KEY_PRODUCER_EPOCH.to_string(), epoch.to_string()),
            (KEY_VERSION.to_string(), version.to_string()),
            (
                KEY_DESCRIPTIONS.to_string(),
                r#"{"a":"old a","b":"old b"}"#.to_string(),
            ),
            (KEY_SYSTEMINFO.to_string(), r#"{"host":"old"}"#.to_string()),
        ])
    }

    fn restart(epoch: &str, from_ts: u64) -> Restart {
        Restart {
            epoch: epoch.to_string(),
            from_ts: Some(from_ts),
        }
    }

    fn agent(version: &str) -> Agent {
        Agent {
            version: Some(version.to_string()),
            systeminfo: Some(r#"{"host":"new"}"#.to_string()),
            descriptions: Some(r#"{"b":"new b","c":"new c"}"#.to_string()),
        }
    }

    fn epochs(patch: &BTreeMap<String, String>) -> Value {
        serde_json::from_str(&patch[KEY_PRODUCER_EPOCHS]).unwrap()
    }

    #[test]
    fn a_first_restart_seeds_the_history_with_the_opening_epoch() {
        let patch = metadata_patch(
            &opened("e1", "6.0.0"),
            100,
            &[restart("e2", 500)],
            &agent("6.0.1"),
            &|_| false,
        );
        assert_eq!(
            epochs(&patch),
            json!([
                {"epoch": "e1", "from_ts": 100, "version": "6.0.0"},
                {"epoch": "e2", "from_ts": 500, "version": "6.0.1"},
            ])
        );
        assert_eq!(patch[KEY_PRODUCER_EPOCH], "e2");
        assert_eq!(patch[KEY_VERSION], "6.0.1");
        assert_eq!(patch[KEY_SYSTEMINFO], r#"{"host":"new"}"#);
    }

    #[test]
    fn descriptions_keep_the_old_metrics_and_take_the_new_text() {
        let patch = metadata_patch(
            &opened("e1", "6.0.0"),
            100,
            &[restart("e2", 500)],
            &agent("6.0.1"),
            &|_| false,
        );
        let d: Value = serde_json::from_str(&patch[KEY_DESCRIPTIONS]).unwrap();
        assert_eq!(d, json!({"a": "old a", "b": "new b", "c": "new c"}));
    }

    #[test]
    fn a_later_restart_appends_to_the_history_already_written() {
        let mut current = opened("e1", "6.0.0");
        current.extend(metadata_patch(
            &current,
            100,
            &[restart("e2", 500)],
            &agent("6.0.1"),
            &|_| false,
        ));
        let patch = metadata_patch(
            &current,
            100,
            &[restart("e3", 900)],
            &agent("6.0.2"),
            &|_| false,
        );
        let e = epochs(&patch);
        assert_eq!(e.as_array().unwrap().len(), 3);
        assert_eq!(
            e[2],
            json!({"epoch": "e3", "from_ts": 900, "version": "6.0.2"})
        );
        assert_eq!(e[0]["from_ts"], 100);
    }

    #[test]
    fn two_restarts_before_one_fetch_leave_the_middle_epoch_without_a_version() {
        let patch = metadata_patch(
            &opened("e1", "6.0.0"),
            100,
            &[restart("e2", 500), restart("e3", 700)],
            &agent("6.0.2"),
            &|_| false,
        );
        assert_eq!(
            epochs(&patch),
            json!([
                {"epoch": "e1", "from_ts": 100, "version": "6.0.0"},
                {"epoch": "e2", "from_ts": 500},
                {"epoch": "e3", "from_ts": 700, "version": "6.0.2"},
            ])
        );
    }

    #[test]
    fn a_version_the_user_set_is_kept() {
        let patch = metadata_patch(
            &opened("e1", "pinned"),
            100,
            &[restart("e2", 500)],
            &agent("6.0.1"),
            &|k| k == KEY_VERSION,
        );
        assert!(!patch.contains_key(KEY_VERSION));
        assert_eq!(patch[KEY_PRODUCER_EPOCH], "e2");
    }

    #[test]
    fn an_agent_that_reported_nothing_changes_only_the_epochs() {
        let patch = metadata_patch(
            &opened("e1", "6.0.0"),
            100,
            &[restart("e2", 500)],
            &Agent::default(),
            &|_| false,
        );
        let keys: Vec<&str> = patch.keys().map(String::as_str).collect();
        assert_eq!(keys, [KEY_PRODUCER_EPOCH, KEY_PRODUCER_EPOCHS]);
        assert_eq!(epochs(&patch)[1], json!({"epoch": "e2", "from_ts": 500}));
    }

    #[test]
    fn a_recording_opened_without_an_epoch_starts_its_history_at_the_restart() {
        let current = BTreeMap::from([(KEY_VERSION.to_string(), "6.0.0".to_string())]);
        let patch = metadata_patch(
            &current,
            100,
            &[restart("e2", 500)],
            &agent("6.0.1"),
            &|_| false,
        );
        assert_eq!(
            epochs(&patch),
            json!([{"epoch": "e2", "from_ts": 500, "version": "6.0.1"}])
        );
    }

    #[test]
    fn the_key_matches_dendro() {
        assert_eq!(KEY_PRODUCER_EPOCHS, dendro::keys::PRODUCER_EPOCHS);
    }
}
