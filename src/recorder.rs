use backtrace::Backtrace;
use metriken_exposition::{MsgpackToParquet, ParquetOptions};
use reqwest::Client;
use reqwest::Url;
use ringlog::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tempfile::tempfile_in;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

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
                .help("Sampling interval. Defaults to 1 second"),
        )
        .arg(
            clap::Arg::new("DURATION")
                .long("duration")
                .action(clap::ArgAction::Set)
                .value_name("DURATION")
                .help("Limits the collection to the provided duration."),
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

    let duration: Option<Duration> = {
        if let Some(duration) = matches.get_one::<String>("DURATION") {
            match duration.parse::<humantime::Duration>() {
                Ok(c) => Some(c.into()),
                Err(error) => {
                    eprintln!("duration is not valid: {duration}\n{error}");
                    std::process::exit(1);
                }
            }
        } else {
            None
        }
    };

    // parse source address
    let mut url: Url = {
        let source = matches.get_one::<String>("SOURCE").unwrap();

        let source = if source.starts_with("http://") || source.starts_with("https://") {
            source.to_string()
        } else {
            format!("http://{source}")
        };

        match source.parse::<Url>() {
            Ok(c) => c,
            Err(error) => {
                eprintln!("source is not a valid URL: {source}\n{error}");
                std::process::exit(1);
            }
        }
    };

    if url.path() != "/" {
        eprintln!("URL should not have an non-root path: {url}");
        std::process::exit(1);
    }

    url.set_path("/metrics/binary");

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
        recorder(url, destination, temporary, interval, duration).await;
    });
}

async fn recorder(
    url: Url,
    destination: std::fs::File,
    temporary: std::fs::File,
    interval: Duration,
    duration: Option<Duration>,
) {
    let mut temporary = tokio::fs::File::from_std(temporary);

    let mut interval = tokio::time::interval(interval);

    let mut client = None;

    let start = std::time::Instant::now();

    while RUNNING.load(Ordering::Relaxed) {
        if let Some(duration) = duration {
            if start.elapsed() >= duration {
                break;
            }
        }

        if client.is_none() {
            debug!("connecting to Rezolus at: {url}");

            match Client::builder()
                .http1_only()
                .build() {
                    Ok(c) => client = Some(c),
                    Err(e) => {
                        error!("error connecting to Rezolus: {e}");
                    }
                }

            continue;
        }

        let c = client.take().unwrap();

        interval.tick().await;

        let start = Instant::now();

        if let Ok(response) = c.get(url.clone()).send().await {
            if let Ok(body) = response.bytes().await {
                let latency = start.elapsed();

                debug!("sampling latency: {} us", latency.as_micros());

                if let Err(e) = temporary.write_all(&body).await {
                    error!("error writing to temporary file: {e}");
                    std::process::exit(1);
                }

                debug!("wrote: {} bytes", body.len());

                debug!("recording latency: {} us", start.elapsed().as_micros());

                client = Some(c);
            }
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
