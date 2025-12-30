use super::*;
use crate::Url;

#[derive(Deserialize)]
pub struct General {
    // how often to sample from the agent
    #[serde(default = "interval")]
    interval: String,

    // duration for the ringbuffer
    #[serde(default = "duration")]
    duration: String,

    // the address of the Rezolus agent
    #[serde(default = "source")]
    source: String,

    // the path for output file
    #[serde(default = "output")]
    output: String,

    // optional HTTP listen address for dump endpoint
    listen: Option<String>,
}

impl Default for General {
    fn default() -> Self {
        Self {
            interval: interval(),
            duration: duration(),
            source: source(),
            output: output(),
            listen: None,
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

    pub fn output(&self) -> PathBuf {
        self.output.clone().into()
    }

    pub fn interval(&self) -> humantime::Duration {
        self.interval.parse().unwrap()
    }

    pub fn duration(&self) -> humantime::Duration {
        self.duration.parse().unwrap()
    }

    pub fn source(&self) -> SocketAddr {
        self.source
            .to_socket_addrs()
            .map_err(|e| {
                eprintln!("bad source address: {e}");
                std::process::exit(1);
            })
            .unwrap()
            .next()
            .ok_or_else(|| {
                eprintln!("could not resolve source socket addr");
                std::process::exit(1);
            })
            .unwrap()
    }

    pub fn url(&self) -> Url {
        let source = self.source();
        Url::try_from(format!("http://{source}/metrics/binary").as_str()).unwrap()
    }

    pub fn listen(&self) -> Option<SocketAddr> {
        self.listen.as_ref().map(|s| {
            s.to_socket_addrs()
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
        })
    }
}
