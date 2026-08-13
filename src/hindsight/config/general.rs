use super::*;
use crate::Url;

#[derive(Deserialize)]
pub struct General {
    #[serde(default = "interval")]
    interval: String,

    #[serde(default = "duration")]
    duration: String,

    // the address of the Rezolus agent
    #[serde(default = "source")]
    source: String,

    #[serde(default = "output")]
    output: String,

    // optional HTTP listen address for dump endpoint
    listen: Option<String>,

    // How many rows a table accumulates before its segment is sealed. Unset
    // means the writer's own default (4096), which is what every deployment
    // should use.
    segment_rows: Option<usize>,
}

impl Default for General {
    fn default() -> Self {
        Self {
            interval: interval(),
            duration: duration(),
            source: source(),
            output: output(),
            listen: None,
            segment_rows: None,
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

    /// Rows per sealed segment, or `None` for the writer's default.
    ///
    /// Segment size is a latency/granularity trade the buffer otherwise makes
    /// for you: bigger segments amortize the per-seal cost, smaller ones bound
    /// how much a dump has to carry whole at each edge (retention and ranged
    /// dumps both work in whole segments) and how long a row waits in the WAL
    /// before it is sealed. At the 1 s default interval 4096 rows is a segment
    /// per ~68 minutes, which is right for a 15 m lookback and wrong for a
    /// buffer scraped ten times a second.
    pub fn segment_rows(&self) -> Option<usize> {
        self.segment_rows
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
