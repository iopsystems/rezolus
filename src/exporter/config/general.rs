use super::*;
use crate::Url;

#[derive(Deserialize)]
pub struct General {
    // the exporter samples periodically, this controls that interval
    #[serde(default = "interval")]
    interval: String,

    // the listen address of the exporter
    #[serde(default = "listen")]
    listen: String,

    // the address of the Rezolus agent
    #[serde(default = "source")]
    source: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            interval: interval(),
            listen: listen(),
            source: source(),
        }
    }
}

impl General {
    pub fn check(&self) {
        if let Err(e) = self.interval.parse::<humantime::Duration>() {
            eprintln!("prometheus sample interval couldn't be parsed: {e}");
            std::process::exit(1);
        }
    }

    pub fn interval(&self) -> humantime::Duration {
        self.interval.parse().unwrap()
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
                eprintln!("could not resolve listen socket addr");
                std::process::exit(1);
            })
            .unwrap()
    }

    pub fn target(&self) -> SocketAddr {
        self.source
            .to_socket_addrs()
            .map_err(|e| {
                eprintln!("bad target address: {e}");
                std::process::exit(1);
            })
            .unwrap()
            .next()
            .ok_or_else(|| {
                eprintln!("could not resolve target socket addr");
                std::process::exit(1);
            })
            .unwrap()
    }

    pub fn mpk_url(&self) -> Url {
        let target = self.target();
        Url::try_from(format!("http://{target}/metrics/binary").as_str()).unwrap()
    }

    pub fn json_url(&self) -> Url {
        let target = self.target();
        Url::try_from(format!("http://{target}/metrics/json").as_str()).unwrap()
    }
}
