use backtrace::Backtrace;
use http::{Method, Version};
use metriken_exposition::{MsgpackToParquet, ParquetOptions};
use ringlog::*;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tempfile::tempfile_in;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;

static RUNNING: AtomicBool = AtomicBool::new(true);

fn main() {
    // custom panic hook to terminate whole process after unwinding
    std::panic::set_hook(Box::new(|s| {
        eprintln!("{s}");
        eprintln!("{:?}", Backtrace::new());
        std::process::exit(101);
    }));

    // parse command line options
    let matches = clap::Command::new(env!("CARGO_BIN_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .long_about(
            "Rezolus recorder periodically samples Rezolus to produce a parquet file of metrics.",
        )
        .arg(
            clap::Arg::new("INTERVAL")
                .long("interval")
                .action(clap::ArgAction::Set)
                .value_name("INTERVAL")
                .help("Sampling interval"),
        )
        .arg(
            clap::Arg::new("VERBOSE")
                .short('v')
                .long("verbose")
                .action(clap::ArgAction::Count)
                .value_name("VERBOSE")
                .help("Increase logging verbosity by one level"),
        )
        .arg(
            clap::Arg::new("SOURCE")
                .help("Rezolus address")
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
        .arg(
            clap::Arg::new("DESTINATION")
                .help("Parquet output file")
                .action(clap::ArgAction::Set)
                .required(true)
                .index(2),
        )
        .get_matches();

    let interval: Duration = {
        let interval = matches
            .get_one::<String>("INTERVAL")
            .map(|v| v.to_string())
            .unwrap_or("1s".to_string());
        match interval.parse::<humantime::Duration>() {
            Ok(c) => c.into(),
            Err(error) => {
                eprintln!("interval is not valid: {interval}\n{error}");
                std::process::exit(1);
            }
        }
    };

    // parse source address
    let addr: SocketAddr = {
        let source = matches.get_one::<String>("SOURCE").unwrap();
        match source.parse::<SocketAddr>() {
            Ok(c) => c,
            Err(error) => {
                eprintln!("source is not a socket: {source}\n{error}");
                std::process::exit(1);
            }
        }
    };

    // convert destination to a path
    let path: PathBuf = {
        let path = matches.get_one::<String>("DESTINATION").unwrap();
        match path.parse() {
            Ok(p) => p,
            Err(error) => {
                eprintln!("destination is not a valid path: {path}\n{error}");
                std::process::exit(1);
            }
        }
    };

    // open destination file
    let destination: std::fs::File = {
        match std::fs::File::create(path.clone()) {
            Ok(f) => f,
            Err(error) => {
                eprintln!("could not open destination: {:?}\n{error}", path);
                std::process::exit(1);
            }
        }
    };

    // open temporary (intermediate msgpack) file
    let mut temp_path = path.clone();
    temp_path.pop();
    let temporary = match tempfile_in(temp_path.clone()) {
        Ok(t) => t,
        Err(error) => {
            eprintln!("could not open temporary file in: {:?}\n{error}", temp_path);
            std::process::exit(1);
        }
    };

    // configure debug log
    let debug_output: Box<dyn Output> = Box::new(Stderr::new());

    let level = match matches.get_count("VERBOSE") {
        0 => Level::Info,
        1 => Level::Debug,
        _ => Level::Trace,
    };

    let debug_log = if level <= Level::Info {
        LogBuilder::new().format(ringlog::default_format)
    } else {
        LogBuilder::new()
    }
    .output(debug_output)
    .build()
    .expect("failed to initialize debug log");

    let mut log = MultiLogBuilder::new()
        .level_filter(level.to_level_filter())
        .default(debug_log)
        .build()
        .start();

    // initialize async runtime
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    // spawn logging thread
    rt.spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let _ = log.flush();
        }
    });

    ctrlc::set_handler(move || {
        if RUNNING.load(Ordering::SeqCst) {
            eprintln!("finalizing recording... please wait...");
            RUNNING.store(false, Ordering::SeqCst);
        } else {
            eprintln!("terminating...");
            std::process::exit(2);
        }
    })
    .expect("failed to set ctrl-c handler");

    // spawn recorder thread
    rt.block_on(async move {
        recorder(addr, destination, temporary, interval).await;
    });
}

async fn recorder(
    addr: SocketAddr,
    destination: std::fs::File,
    temporary: std::fs::File,
    interval: Duration,
) {
    let mut temporary = tokio::fs::File::from_std(temporary);

    let mut interval = tokio::time::interval(interval);

    let mut client = None;

    while RUNNING.load(Ordering::Relaxed) {
        if client.is_none() {
            debug!("connecting to Rezolus...");

            match TcpStream::connect(addr).await {
                Ok(s) => {
                    if s.set_nodelay(true).is_err() {
                        continue;
                    }

                    debug!("performing http2 handshake...");

                    if let Ok((h2, connection)) = ::h2::client::handshake(s).await {
                        tokio::spawn(async move {
                            let _ = connection.await;
                        });

                        if let Ok(h2) = h2.ready().await {
                            debug!("connection to Rezolus is established");

                            client = Some(h2);
                        }
                    }
                }
                Err(e) => {
                    error!("error connecting to Rezolus: {e}");
                }
            }

            continue;
        }

        let c = client.take().unwrap();

        if let Ok(mut sender) = c.clone().ready().await {
            let request = http::request::Builder::new()
                .version(Version::HTTP_2)
                .method(Method::GET)
                .uri(&format!("http://{addr}/metrics/binary"))
                .body(())
                .unwrap();

            interval.tick().await;

            let start = Instant::now();

            if let Ok((response, _)) = sender.send_request(request, true) {
                if let Ok(response) = response.await {
                    let mut body = response.into_body();

                    let mut temp = Vec::new();

                    while let Some(chunk) = body.data().await {
                        match chunk {
                            Ok(c) => {
                                temp.push(c);
                            }
                            Err(e) => {
                                error!("error sampling: {e}");
                                continue;
                            }
                        }
                    }

                    let latency = start.elapsed();

                    debug!("sampling latency: {}", latency.as_micros());

                    for chunk in temp {
                        if let Err(e) = temporary.write_all(&chunk).await {
                            error!("error writing to temporary file: {e}");
                            std::process::exit(1);
                        }
                    }
                }
            }

            client = Some(c);
        }
    }

    debug!("flushing and seeking to start of temp file");

    let _ = temporary.flush().await;
    let _ = temporary.rewind().await;
    let temporary = temporary.into_std().await;

    debug!("converting temp file to parquet");

    if let Err(e) = MsgpackToParquet::with_options(ParquetOptions::new())
        .convert_file_handle(temporary, destination)
    {
        eprintln!("error saving parquet file: {e}");
    }
}
