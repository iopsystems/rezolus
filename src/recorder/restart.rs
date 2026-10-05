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

/// How many times the restarted agent's metadata is fetched, one tick apart,
/// before the restart is written without it.
pub const FETCH_ATTEMPTS: u32 = 3;

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
/// start of the epoch `current` was opened with, if it has one.
/// `version_pinned` means the user set `version` with `--metadata`, which a
/// restart leaves alone.
///
/// - `producer_epoch` becomes the newest epoch.
/// - `producer_epochs` gains one entry per restart. The first restart seeds it
///   with the epoch the recording opened with, from `first_ts`, when a row
///   from that epoch was recorded before the restart.
/// - `version` and `systeminfo` become the new agent's, when it reported them.
/// - `descriptions` becomes the union of the old and new maps, the new
///   description winning for a metric in both, so the rows from before the
///   restart keep their descriptions.
pub fn metadata_patch(
    current: &BTreeMap<String, String>,
    first_ts: Option<u64>,
    restarts: &[Restart],
    agent: &Agent,
    version_pinned: bool,
) -> BTreeMap<String, String> {
    let mut patch = BTreeMap::new();
    let Some(newest) = restarts.last() else {
        return patch;
    };

    let mut epochs: Vec<Value> = current
        .get(KEY_PRODUCER_EPOCHS)
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    // The opening epoch has rows only if one was recorded before the first
    // restart's first row.
    let opened_with_rows =
        first_ts.filter(|first| restarts[0].from_ts.is_none_or(|from| *first < from));
    if let (true, Some(first), Some(epoch)) = (
        epochs.is_empty(),
        opened_with_rows,
        current.get(KEY_PRODUCER_EPOCH),
    ) {
        // A pinned `version` is the user's label, not what this process
        // reported, so its entry has none.
        let version = (!version_pinned)
            .then(|| current.get(KEY_VERSION))
            .flatten();
        epochs.push(entry(epoch, first, version));
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
            restart.from_ts.or(first_ts).unwrap_or_default(),
            version,
        ));
    }
    patch.insert(
        KEY_PRODUCER_EPOCHS.to_string(),
        Value::Array(epochs).to_string(),
    );
    patch.insert(KEY_PRODUCER_EPOCH.to_string(), newest.epoch.clone());

    if !version_pinned {
        if let Some(version) = &agent.version {
            patch.insert(KEY_VERSION.to_string(), version.clone());
        }
    }
    if let Some(systeminfo) = &agent.systeminfo {
        patch.insert(KEY_SYSTEMINFO.to_string(), systeminfo.clone());
    }
    if let Some(descriptions) =
        merge_descriptions(current.get(KEY_DESCRIPTIONS), agent.descriptions.as_ref())
    {
        patch.insert(KEY_DESCRIPTIONS.to_string(), descriptions);
    }
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
            Some(100),
            &[restart("e2", 500)],
            &agent("6.0.1"),
            false,
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
            Some(100),
            &[restart("e2", 500)],
            &agent("6.0.1"),
            false,
        );
        let d: Value = serde_json::from_str(&patch[KEY_DESCRIPTIONS]).unwrap();
        assert_eq!(d, json!({"a": "old a", "b": "new b", "c": "new c"}));
    }

    #[test]
    fn a_later_restart_appends_to_the_history_already_written() {
        let mut current = opened("e1", "6.0.0");
        current.extend(metadata_patch(
            &current,
            Some(100),
            &[restart("e2", 500)],
            &agent("6.0.1"),
            false,
        ));
        let patch = metadata_patch(
            &current,
            Some(100),
            &[restart("e3", 900)],
            &agent("6.0.2"),
            false,
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
            Some(100),
            &[restart("e2", 500), restart("e3", 700)],
            &agent("6.0.2"),
            false,
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
            Some(100),
            &[restart("e2", 500)],
            &agent("6.0.1"),
            true,
        );
        assert!(!patch.contains_key(KEY_VERSION));
        assert_eq!(patch[KEY_PRODUCER_EPOCH], "e2");
        // The pinned label is not what the first process reported.
        assert_eq!(
            epochs(&patch),
            json!([
                {"epoch": "e1", "from_ts": 100},
                {"epoch": "e2", "from_ts": 500, "version": "6.0.1"},
            ])
        );
    }

    #[test]
    fn an_opening_epoch_with_no_rows_before_the_restart_gets_no_entry() {
        // The first row staged is the new process's: the recording's first
        // stamp and the restart's are the same row.
        for first_ts in [Some(500), None] {
            let patch = metadata_patch(
                &opened("e1", "6.0.0"),
                first_ts,
                &[restart("e2", 500)],
                &agent("6.0.1"),
                false,
            );
            assert_eq!(
                epochs(&patch),
                json!([{"epoch": "e2", "from_ts": 500, "version": "6.0.1"}]),
                "first_ts {first_ts:?}"
            );
        }
    }

    #[test]
    fn an_agent_that_reported_nothing_changes_only_the_epochs() {
        let patch = metadata_patch(
            &opened("e1", "6.0.0"),
            Some(100),
            &[restart("e2", 500)],
            &Agent::default(),
            false,
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
            Some(100),
            &[restart("e2", 500)],
            &agent("6.0.1"),
            false,
        );
        assert_eq!(
            epochs(&patch),
            json!([{"epoch": "e2", "from_ts": 500, "version": "6.0.1"}])
        );
    }

    /// A stand-in agent whose `/status` reports `epoch` and version 6.0.1;
    /// every other route is 404. Returns its base URL.
    fn agent_reporting(epoch: &'static str) -> reqwest::Url {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let response = if req.starts_with("GET /status ") {
                    let body = format!(
                        r#"{{"version":"6.0.1","producer_epoch":"{epoch}","uptime_seconds":1,"ttl_seconds":1,"samplers":[]}}"#
                    );
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        reqwest::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()
    }

    #[test]
    fn a_fetch_is_taken_only_from_the_process_the_restart_is_for() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = reqwest::Client::builder().http1_only().build().unwrap();
        let url = agent_reporting("e2");
        let fetch = |epoch: &'static str| {
            rt.block_on(crate::recorder::fetch_restarted_agent(
                &client,
                &url,
                epoch,
                std::time::Duration::from_secs(5),
            ))
        };
        let agent = fetch("e2").expect("the process the restart is for");
        assert_eq!(agent.version.as_deref(), Some("6.0.1"));
        assert!(
            fetch("e3").is_none(),
            "a process with another epoch answered; its metadata is not this restart's"
        );
    }

    #[test]
    fn the_key_matches_dendro() {
        assert_eq!(KEY_PRODUCER_EPOCHS, dendro::keys::PRODUCER_EPOCHS);
    }
}
