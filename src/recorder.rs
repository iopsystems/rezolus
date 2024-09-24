use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tempfile::tempfile_in;
use std::path::PathBuf;
use http::Version;
use http::Method;
use std::time::Instant;
use tokio::net::TcpStream;
use std::net::SocketAddr;
use backtrace::Backtrace;
use ringlog::*;

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
        .long_about("Rezolus recorder periodically samples Rezolus to produce a parquet file of metrics.")
        .arg(
            clap::Arg::new("SOURCE")
                .help("Rezolus address")
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
        .arg(
            clap::Arg::new("DESTINATION")
                .help("Parquet file")
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
        .get_matches();

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
        match std::fs::File::open(path.clone()) {
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

    let level = Level::Info;

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

    // spawn recorder thread
    rt.spawn(async move {
        recorder(addr, destination, temporary).await;
    });


    // let mut samplers = Vec::new();

    // for init in SAMPLERS {
    //     if let Ok(Some(s)) = init(config.clone()) {
    //         samplers.push(s);
    //     }
    // }

    // let samplers = Arc::new(samplers.into_boxed_slice());

    // rt.spawn(async move {
    //     exposition::http::serve(config, samplers).await;
    // });

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

async fn recorder(addr: SocketAddr, _destination: std::fs::File, temporary: std::fs::File) {
    let mut temporary = tokio::fs::File::from_std(temporary);

    let mut interval = tokio::time::interval(Duration::from_millis(1000));


    let mut client = None;

    loop {
        if client.is_none() {
            if let Ok(s) = TcpStream::connect(addr).await {
                if s.set_nodelay(true).is_err() {
                    continue;
                }

                if let Ok((h2, connection)) = ::h2::client::handshake(s).await {
                    tokio::spawn(async move {
                        let _ = connection.await;
                    });

                    if let Ok(h2) = h2.ready().await {
                        client = Some(h2);
                    }
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

                    info!("sampling latency: {}", latency.as_micros());

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


}
