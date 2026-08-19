use super::*;

fn default_snapshot_format() -> SnapshotFormat {
    SnapshotFormat::V2
}

/// The wire format used for metric snapshots served on `/metrics/binary`.
///
/// `V2` is today's flat per-metric format. `V3` emits acquisition-group
/// snapshots. This is transitional: the default flips to `V3` once samplers
/// migrate; see docs/journal/2026-08-17-window-sidecar-cost.md.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotFormat {
    #[default]
    V2,
    V3,
}

#[derive(Deserialize, Default)]
pub struct General {
    #[serde(default = "listen")]
    listen: String,

    // the agent caches metrics snapshots with the following TTL to prevent
    // excessive resource utilization
    #[serde(default = "ttl")]
    ttl: String,

    // path to external BTF file for BPF programs (optional)
    #[serde(default)]
    btf_path: Option<String>,

    // wire format for metric snapshots served on `/metrics/binary` (see
    // `SnapshotFormat` for details); transitional, defaults to v2
    #[serde(default = "default_snapshot_format")]
    snapshot_format: SnapshotFormat,
}

impl General {
    pub fn check(&self) {
        if let Err(e) = self.ttl.parse::<humantime::Duration>() {
            eprintln!("ttl couldn't be parsed: {e}");
            std::process::exit(1);
        }

        if let Some(ref btf_path) = self.btf_path {
            if !std::path::Path::new(btf_path).exists() {
                eprintln!("BTF file not found: {btf_path}");
                std::process::exit(1);
            }
        }
    }

    pub fn listen(&self) -> SocketAddr {
        self.listen
            .to_socket_addrs()
            .map_err(|e| {
                eprintln!("bad listen address: {e}");
                std::process::exit(1);
            })
            .unwrap()
            .next()
            .ok_or_else(|| {
                eprintln!("could not resolve socket addr");
                std::process::exit(1);
            })
            .unwrap()
    }

    pub fn ttl(&self) -> std::time::Duration {
        *self.ttl.parse::<humantime::Duration>().unwrap()
    }

    #[allow(dead_code)]
    pub fn btf_path(&self) -> Option<&str> {
        self.btf_path.as_deref()
    }

    // wired to the snapshot builder (next task)
    #[allow(dead_code)]
    pub fn snapshot_format(&self) -> SnapshotFormat {
        self.snapshot_format
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn general(toml: &str) -> General {
        toml::from_str(toml).expect("valid config")
    }

    #[test]
    fn snapshot_format_v3_parses() {
        let g = general("snapshot_format = \"v3\"\n");
        assert_eq!(g.snapshot_format(), SnapshotFormat::V3);
    }

    #[test]
    fn snapshot_format_defaults_to_v2_when_absent() {
        let g = general("");
        assert_eq!(g.snapshot_format(), SnapshotFormat::V2);
    }

    #[test]
    fn snapshot_format_rejects_unknown_value() {
        let result: Result<General, _> = toml::from_str("snapshot_format = \"v9\"\n");
        assert!(result.is_err());
    }
}
