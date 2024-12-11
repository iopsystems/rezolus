use backtrace::Backtrace;
use clap::Parser;
use clap::ValueEnum;
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

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum Format {
    Parquet,
    Raw,
}

#[derive(Parser)]
#[command(version)]
#[command(about = "An on-demand tool for recording Rezolus metrics to a file", long_about = None)]
struct Config {
    #[arg(short, long, default_value_t = humantime::Duration::from(Duration::from_secs(1)))]
    interval: humantime::Duration,
    #[arg(short, long)]
    duration: Option<humantime::Duration>,
    #[arg(short, long, value_enum, default_value_t = Format::Parquet)]
    output_format: Format,
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
    #[arg(value_name = "SOURCE")]
    source: String,
    #[arg(value_name = "FILE")]
    destination: String,
}

impl Config {
    /// Opens the destination file. This will be the final output file.
    fn destination(&self) -> std::fs::File {
        match std::fs::File::create(self.destination_path()) {
            Ok(f) => f,
            Err(error) => {
                eprintln!(
                    "could not open destination: {:?}\n{error}",
                    self.destination
                );
                std::process::exit(1);
            }
        }
    }

    /// Get the path to the destination file.
    fn destination_path(&self) -> PathBuf {
        match self.destination.parse() {
            Ok(p) => p,
            Err(error) => {
                eprintln!(
                    "destination is not a valid path: {}\n{error}",
                    self.destination
                );
                std::process::exit(1);
            }
        }
    }

    /// Get the duration for time-limited run. If this is `None` we sample until
    /// ctrl-c
    fn duration(&self) -> Option<Duration> {
        self.duration.map(|v| v.into())
    }

    /// The interval between each sample.
    fn interval(&self) -> Duration {
        self.interval.into()
    }

    /// An optional temporary file. Some formats may record directly to the
    /// destination while others may need post-processing to transform from our
    /// raw format.
    fn temporary(&self) -> Option<tokio::fs::File> {
        if self.output_format != Format::Raw {
            // tempfile will be in same directory as out destination file
            let mut temp_path = self.destination_path();
            temp_path.pop();

            let temporary = match tempfile_in(temp_path.clone()) {
                Ok(t) => t,
                Err(error) => {
                    eprintln!("could not open temporary file in: {:?}\n{error}", temp_path);
                    std::process::exit(1);
                }
            };

            Some(tokio::fs::File::from_std(temporary))
        } else {
            None
        }
    }

    /// The url to request. Currently we expect that if this is a complete URL
    /// that the path is root-level. We accept host:port, or IP:port here too.
    /// We then sample `/metrics/binary` which is the Rezolus msgpack endpoint.
    fn url(&self) -> Url {
        // parse source address
        let mut url: Url = {
            let source = self.source.clone();

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

        url
    }
}

fn main() {
    // custom panic hook to terminate whole process after unwinding
    std::panic::set_hook(Box::new(|s| {
        eprintln!("{s}");
        eprintln!("{:?}", Backtrace::new());
        std::process::exit(101);
    }));

    // parse command line options
    let config = Config::parse();

    // configure debug log
    let debug_output: Box<dyn Output> = Box::new(Stderr::new());

    let level = match config.verbose {
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
        recorder(config).await;
    });
}

async fn recorder(config: Config) {
    // load the url to connect to
    let url = config.url();

    // open destination and (optional) temporary files
    let mut destination = Some(config.destination());
    let mut temporary = config.temporary();

    // our http client
    let mut client = None;

    // sampling interval
    let mut interval = tokio::time::interval(config.interval());

    // start time
    let start = std::time::Instant::now();

    // writer will be either the temporary file or the final destination file
    // depending on the output format
    let mut writer = temporary
        .take()
        .unwrap_or_else(|| destination.take().unwrap().into());

    // sample in a loop until RUNNING is false or duration has completed
    while RUNNING.load(Ordering::Relaxed) {
        // check if the duration has completed
        if let Some(duration) = config.duration() {
            if start.elapsed() >= duration {
                break;
            }
        }

        // connect to rezolus
        if client.is_none() {
            debug!("connecting to Rezolus at: {url}");

            match Client::builder().http1_only().build() {
                Ok(c) => client = Some(c),
                Err(e) => {
                    error!("error connecting to Rezolus: {e}");
                }
            }

            continue;
        }

        let c = client.take().unwrap();

        // wait to sample
        interval.tick().await;

        let start = Instant::now();

        // sample rezolus
        if let Ok(response) = c.get(url.clone()).send().await {
            if let Ok(body) = response.bytes().await {
                let latency = start.elapsed();

                debug!("sampling latency: {} us", latency.as_micros());

                if let Err(e) = writer.write_all(&body).await {
                    error!("error writing to temporary file: {e}");
                    std::process::exit(1);
                }

                client = Some(c);
            }
        }
    }

    debug!("flushing writer");
    let _ = writer.flush().await;

    // handle any output format specific transforms
    match config.output_format {
        Format::Raw => {
            debug!("finished");
        }
        Format::Parquet => {
            debug!("converting temp file to parquet");

            let _ = writer.rewind().await;
            let temporary = writer.into_std().await;

            if let Err(e) = MsgpackToParquet::with_options(ParquetOptions::new())
                .convert_file_handle(temporary, destination.unwrap())
            {
                eprintln!("error saving parquet file: {e}");
            }
        }
    }
}
