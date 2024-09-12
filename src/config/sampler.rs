use crate::config::*;

#[derive(Deserialize, Default)]
pub struct Sampler {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    ttl: Option<String>,
}

impl Sampler {
    pub fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    pub fn ttl(&self) -> Option<&String> {
        self.ttl.as_ref()
    }

    pub fn check(&self, name: &str) {
        if let Some(Err(e)) = self.ttl.as_ref().map(|v| v.parse::<humantime::Duration>()) {
            eprintln!("{name} sampler ttl is not valid: {e}");
            std::process::exit(1);
        }
    }
}
