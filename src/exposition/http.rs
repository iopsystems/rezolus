// Copyright 2019 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0


use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::*;
// use rustcommon_logger::*;
use tiny_http::{Method, Response, Server};

use super::MetricsSnapshot;

pub struct Http {
    snapshot: MetricsSnapshot,
    server: Server,
    updated: Instant,
    header: Option<tiny_http::Header>,
}

impl Http {
    pub fn new(config: Arc<Config>, metrics: Arc<Metrics>, ) -> Self {
        let address = config.listen().expect("no listen address");
        let server = tiny_http::Server::http(address);
        if server.is_err() {
            fatal!("Failed to open {} for HTTP Stats listener", address);
        }
        let count_label = config.general().reading_suffix();
        let header = config.general().http_header()
            .map(|s| tiny_http::Header::from_str(&s).expect("invalid HTTP header")); 

        Self {
            snapshot: MetricsSnapshot::new(metrics, count_label),
            server: server.unwrap(),
            updated: Instant::now(),
            header,
        }
    }

    pub fn run(&mut self) {
        if let Ok(Some(request)) = self.server.try_recv() {
            if self.updated.elapsed() >= Duration::from_millis(500) {
                self.snapshot.refresh();
                self.updated = Instant::now();
            }
            let url = request.url();
            let parts: Vec<&str> = url.split('?').collect();
            let url = parts[0];

            match request.method() {
                Method::Get => { 
                    let content = match url {
                        "/" => {
                            debug!("Serving GET on index");
                            format!(
                                "Welcome to {}\nVersion: {}\n",
                                crate::config::NAME,
                                crate::config::VERSION,
                            )
                        }
                        "/metrics" => {
                            debug!("Serving Prometheus compatible stats");
                            self.snapshot.prometheus()
                        }
                        "/metrics.json" | "/vars.json" | "/admin/metrics.json" => {
                            debug!("Serving machine readable stats");
                            self.snapshot.json(false)
                        }
                        "/vars" => {
                            debug!("Serving human readable stats");
                            self.snapshot.human()
                        }
                        url => {
                            debug!("GET on non-existent url: {}", url);
                            debug!("Serving machine readable stats");
                            self.snapshot.json(false)
                        }
                    };
                    let mut response = Response::from_string(content);
                    if let Some(header) = &self.header {
                        response = response.with_header(header.clone())
                    }
                    let _ = request.respond(response);
                },
                method => {
                    debug!("unsupported request method: {}", method);
                    let mut response = Response::empty(404);
                    if let Some(header) = &self.header {
                        response = response.with_header(header.clone())
                    }
                    let _ = request.respond(response);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
