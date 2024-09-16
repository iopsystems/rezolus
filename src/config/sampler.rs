use crate::config::*;

#[derive(Deserialize, Default)]
pub struct Sampler {
    #[serde(default)]
    enabled: Option<bool>,
}

impl Sampler {
    pub fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    pub fn check(&self, _name: &str) {}
}
