use super::*;
use crate::Url;

#[derive(Deserialize, Default)]
pub struct General {
    #[serde(default = "listen")]
    listen: String,

    #[serde(default = "target")]
    target: String,
}

impl General {
    pub fn check(&self) {}

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
        self.target
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

    pub fn url(&self) -> Url {
        let target = self.target();
        Url::try_from(format!("http://{target}/metrics/binary").as_str()).unwrap()
    }
}
