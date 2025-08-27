use super::*;

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

    pub fn btf_path(&self) -> Option<&str> {
        self.btf_path.as_deref()
    }
}
