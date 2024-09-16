use crate::config::*;

#[derive(Deserialize)]
pub struct Log {
    #[serde(with = "LevelDef")]
    #[serde(default = "log_level")]
    level: Level,
}

impl Default for Log {
    fn default() -> Self {
        Self { level: log_level() }
    }
}

impl Log {
    pub fn level(&self) -> Level {
        self.level
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
#[serde(remote = "Level")]
#[serde(deny_unknown_fields)]
enum LevelDef {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

fn log_level() -> Level {
    Level::Info
}
