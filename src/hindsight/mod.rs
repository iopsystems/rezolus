use super::*;

use std::fs::OpenOptions;
use std::path::Path;

mod buffer;
mod config;
mod http;
mod state;

use buffer::HindsightBuffer;
pub use config::Config;
use state::{DumpToFileRequest, DumpToFileResponse, SharedState, TimeRange};

pub fn command() -> Command {
    Command::new("hindsight")
        .about("Continuously record to a rolling on-disk buffer for after-the-fact snapshots")
        .long_about(
            "Long-running daemon that pulls from a Rezolus agent and keeps a rolling,\n\
             high-resolution buffer on disk. When an incident happens you snapshot the\n\
             buffer to a `.rez` file — effectively recording the minutes *before* the trigger,\n\
             at a resolution finer than your normal observability stack keeps.\n\n\
             The buffer is an ordinary `.rez` recording with retention: everything older than\n\
             the lookback is evicted every tick, so the file stays bounded. It is readable\n\
             while it is being written — `rezolus view`, the MCP tools and\n\
             `rezolus recording metadata` all open it live — and a snapshot is a consistent\n\
             point-in-time copy taken without pausing the recording.\n\n\
             Configuration is a TOML file (the only argument). It sets the sampling interval\n\
             ([general] interval, e.g. 1s), how far back the buffer reaches ([general] duration,\n\
             e.g. 15m), the agent to read from ([general] source), and the snapshot output path\n\
             ([general] output). See config/hindsight.toml for a documented starting point.\n\n\
             TRIGGERING A SNAPSHOT: send SIGHUP to write the buffer to the output file without\n\
             stopping the daemon. Optionally set [general] listen to enable an HTTP endpoint for\n\
             remote status/dump requests instead. Either way the recording keeps running for the\n\
             whole of the snapshot — a capture costs no samples, including the samples taken\n\
             while it is being written.\n\n\
             EXAMPLE:\n    \
             # Run the rolling-buffer daemon using the example config\n    \
             rezolus hindsight config/hindsight.toml",
        )
        .arg(
            clap::Arg::new("CONFIG")
                .help("Path to the hindsight TOML config (e.g. config/hindsight.toml); see that file for interval/duration/source/output")
                .value_parser(value_parser!(PathBuf))
                .action(clap::ArgAction::Set)
                .required(true)
                .index(1),
        )
}

/// Runs the Rezolus `flight-recorder`: a Rezolus client that pulls from the
/// agent's msgpack endpoint and keeps a rolling `.rez` buffer covering the
/// configured lookback. On SIGHUP it writes the buffer out to the output file.
///
/// This is intended to be run as a daemon that allows retroactive collection of
/// high-resolution metrics in the event of an anomaly. To be effective the
/// collection `interval` should be more frequent than your observability stack
/// allows for, for example per-second collection in an environment with only
/// minutely metrics. Additionally the `duration` should allow adequate time to
/// not only cover the duration of an anomalous event but give time for an
/// engineer or automated process to respond and trigger a snapshot.
///
/// The buffer is the same streaming `.rez` v3 writer the recorder uses, with
/// retention configured — see [`buffer`]. That is what replaced the fixed-size
/// ring of 4 KB slots: a ring nothing but hindsight could read, whose dump
/// copied a buffer that was being overwritten in place and could therefore
/// tear.
///
/// Optionally, an HTTP endpoint can be enabled to allow remote triggering of
/// snapshots without terminating the service.
pub fn run(config: Config) {
    let config: Arc<Config> = config.into();

    let _log_drain = configure_logging(config.log().level().to_tracing_level());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(1)
        .thread_name("rezolus")
        .build()
        .expect("failed to launch async runtime");

    // Wakes the recording loop when a signal changes STATE, so a capture starts
    // on the signal rather than on the next tick. The loop reads STATE itself —
    // this only says "look again" — so a dropped or full channel costs nothing
    // but the tick of latency the loop used to have anyway.
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel::<()>(1);

    ctrlc::set_handler(move || {
        let state = STATE.load(Ordering::SeqCst);

        if state == RUNNING {
            info!("triggering buffer capture");
            STATE.store(CAPTURING, Ordering::SeqCst);
        } else if state == CAPTURING {
            info!("waiting for capture to complete before exiting");
            STATE.store(TERMINATING, Ordering::SeqCst);
        } else {
            info!("terminating immediately");
            std::process::exit(2);
        }

        let _ = signal_tx.try_send(());
    })
    .expect("failed to set ctrl-c handler");

    let url = config.general().url();

    let blocking_client = match reqwest::blocking::Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            error!("error connecting to Rezolus: {e}");
            std::process::exit(1);
        }
    };

    let fetch = |path: &str| -> Option<String> {
        let mut u = url.clone();
        u.set_path(path);
        blocking_client
            .get(u)
            .send()
            .ok()
            .filter(|r| r.status().is_success())
            .and_then(|r| r.text().ok())
    };

    let agent_systeminfo = fetch("/systeminfo");
    let agent_descriptions = fetch("/metrics/descriptions");

    if agent_systeminfo.is_some() {
        debug!("fetched systeminfo from agent");
    } else {
        debug!("agent systeminfo not available");
    }

    let async_client = match reqwest::Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            error!("error connecting to Rezolus: {e}");
            std::process::exit(1);
        }
    };

    let output = config.general().output();

    // Fail fast rather than after fifteen minutes of buffering: if the output
    // cannot be written there is no point recording anything to dump into it.
    if let Err(e) = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&output)
    {
        error!("failed to open destination file: {e}");
        std::process::exit(1);
    }
    if output.extension().is_some_and(|e| e == "parquet") {
        warn!(
            "{} will be written as a .rez archive, not parquet — hindsight snapshots \
             are `.rez` recordings since v3",
            output.display()
        );
    }

    let buffer_dir = {
        let mut path = output.clone();
        path.pop();
        path
    };

    // The buffer lives in a private directory beside the output, so its
    // `-wal`/`-shm` sidecars cannot collide with anything and the whole lot is
    // removed together when the daemon exits cleanly.
    let staging = match tempfile::TempDir::new_in(&buffer_dir) {
        Ok(t) => t,
        Err(error) => {
            eprintln!("could not open a buffer directory in: {buffer_dir:?}\n{error}");
            std::process::exit(1);
        }
    };
    let buffer_path = staging.path().join("hindsight.rez");

    // Probe the endpoint once: it must exist, and the sampling interval has to
    // leave room for the scrape it implies.
    let start = Instant::now();
    let latency = if let Ok(response) = blocking_client.get(url.clone()).send() {
        if let Ok(body) = response.bytes() {
            let latency = start.elapsed();
            debug!("sampling latency: {} us", latency.as_micros());
            debug!("body size: {}", body.len());
            latency
        } else {
            error!("error reading metrics endpoint");
            std::process::exit(1);
        }
    } else {
        error!("error reading metrics endpoint");
        std::process::exit(1);
    };

    if config.general().interval().as_micros() < (latency.as_micros() * 2) {
        error!("the sampling interval is too short to reliably record");
        error!(
            "set the interval to at least: {} us",
            latency.as_micros() * 2
        );
        std::process::exit(1);
    }

    let interval_dur: Duration = config.general().interval().into();
    let lookback: Duration = config.general().duration().into();

    // Row stamps are `anchor + monotonic elapsed`, exactly as in the recorder,
    // so a wall-clock step cannot bake a decreasing timestamp into a sealed
    // segment; the raw reading rides along as a per-row observation instead.
    let clock_anchor_wall_ns = wall_ns();
    let clock_anchor_mono = Instant::now();

    let seed = crate::recorder::rez_v3_writer::ManifestSeed {
        labels: crate::recorder::rez::build_labels("rezolus", agent_systeminfo.as_deref(), &[]),
        metadata: buffer_metadata(interval_dur, &agent_systeminfo, &agent_descriptions),
        clock_anchor_wall_ns,
    };

    // Segment size tracks the scrape interval rather than being fixed: the
    // writer's 900 rows is a segment per ~15 minutes at the default 1 s
    // interval, which a faster buffer wants smaller. Everything else about the
    // seal policy — the byte cap and the age cap — stays the writer's.
    let mut policy = crate::recorder::seal_policy::SealPolicy::default();
    if let Some(rows) = config.general().segment_rows() {
        // `max(1)`: a zero row target would seal a one-row segment every tick
        // forever rather than doing anything useful with the 0.
        policy.max_rows = rows.max(1);
    }

    let mut buffer = match HindsightBuffer::create(&buffer_path, seed, lookback, policy) {
        Ok(b) => b,
        Err(e) => {
            error!("failed to create the hindsight buffer: {e}");
            std::process::exit(1);
        }
    };
    info!(
        "buffering {} of metrics at {} in {}",
        humantime::format_duration(lookback),
        humantime::format_duration(interval_dur),
        buffer_path.display()
    );

    let shared_state = Arc::new(SharedState::new(
        buffer_path.clone(),
        output.clone(),
        interval_dur,
        lookback,
    ));

    let (dump_tx, mut dump_rx) = tokio::sync::mpsc::channel::<DumpToFileRequest>(8);

    if let Some(listen_addr) = config.general().listen() {
        let shared = shared_state.clone();
        rt.spawn(async move {
            http::serve(listen_addr, shared, dump_tx).await;
        });
    }

    rt.block_on(async move {
        let mut interval = crate::common::aligned_interval(interval_dur);

        // Dumps run OFF this loop — that is the whole shape of what follows.
        // Every dump is spawned and its result comes back asynchronously,
        // because a `select!` does not poll its other branches while a handler
        // body is awaiting, and `MissedTickBehavior::Skip` DISCARDS the ticks
        // that go by in the meantime rather than deferring them. A dump taken
        // inline therefore does not delay samples, it deletes them — worst on
        // the large buffers an incident is captured from, and precisely over
        // the minutes after the trigger when the incident is still unfolding.
        let mut dumps = tokio::task::JoinSet::new();
        // One dump at a time, whatever triggered it: they all write the same
        // output path, and serializing keeps the guarantee the in-loop version
        // gave for free — a caller is told about a file holding its own copy,
        // not one another dump renamed into place a moment later. Waiting for
        // the gate happens on the spawned task, so the loop keeps ticking.
        let dump_gate = Arc::new(tokio::sync::Mutex::new(()));
        // The SIGHUP/ctrl-c capture, which has no caller to reply to: it
        // reports back here so its completion is logged from the loop and the
        // state machine advances in one place.
        let (capture_tx, mut capture_rx) = tokio::sync::mpsc::channel::<DumpToFileResponse>(1);
        let mut capturing = false;

        loop {
            tokio::select! {
                biased;

                Some(request) = dump_rx.recv() => {
                    debug!("received dump-to-file request via HTTP");
                    let (buffer_path, output, range) =
                        (buffer_path.clone(), output.clone(), request.time_range);
                    let gate = dump_gate.clone();
                    dumps.spawn(async move {
                        let _serialized = gate.lock().await;
                        // On a blocking thread: a large `VACUUM INTO` must not
                        // park the HTTP server along with the tick.
                        let response = tokio::task::spawn_blocking(move || {
                            dump_to_file(&buffer_path, &output, &range)
                        })
                        .await
                        .unwrap_or_else(|e| {
                            DumpToFileResponse::error(format!("the dump task failed: {e}"))
                        });
                        // The reply moved off the loop with the work; a failure
                        // is still a reply, and still reaches the caller.
                        let _ = request.response_tx.send(response);
                    });
                }

                // Reap finished HTTP dumps so the set cannot grow unbounded.
                // They have already answered their own callers.
                Some(_) = dumps.join_next(), if !dumps.is_empty() => {}

                Some(response) = capture_rx.recv() => {
                    capturing = false;
                    let terminating = STATE.load(Ordering::SeqCst) == TERMINATING;
                    // Back to RUNNING BEFORE the log line, so a second signal
                    // sent on seeing that line reads as a new capture rather
                    // than as "terminate once the capture is done".
                    if !terminating {
                        STATE.store(RUNNING, Ordering::SeqCst);
                    }
                    log_capture(&response);
                    if terminating {
                        break;
                    }
                }

                // A signal changed STATE; the check below acts on it.
                Some(_) = signal_rx.recv() => {}

                _ = interval.tick() => {
                    let start = Instant::now();

                    if let Ok(response) = async_client.get(url.clone()).send().await {
                        if let Ok(body) = response.bytes().await {
                            let latency = start.elapsed();

                            debug!("sampling latency: {} us", latency.as_micros());
                            debug!("body size: {}", body.len());

                            let (anchored_ns, wall_offset_ns) = crate::recorder::anchored_stamp(
                                clock_anchor_wall_ns,
                                clock_anchor_mono.elapsed(),
                                wall_ns(),
                            );

                            // `Snapshot::from_msgpack`, not a bare `from_slice`:
                            // the same depth-capped, trailing-byte-checked
                            // decode as the recorder's `.rez`-mode call site —
                            // hindsight is the same always-on ingest path,
                            // scraping whatever msgpack endpoint it's pointed
                            // at, and is the most exposed process of the two
                            // (it runs unattended, indefinitely).
                            match metriken_exposition::Snapshot::from_msgpack(&body) {
                                Ok(snapshot) => {
                                    if let Err(e) =
                                        buffer.ingest(&snapshot, anchored_ns, wall_offset_ns)
                                    {
                                        fatal(&e, &buffer_path);
                                    }
                                    shared_state.record_tick();
                                }
                                Err(e) => warn!("msgpack decode error: {e}"),
                            }
                        } else {
                            error!("failed to read response");
                            std::process::exit(1);
                        }
                    } else {
                        error!("failed to get metrics");
                        std::process::exit(1);
                    }

                    // Every tick, scrape or not: this is where segments
                    // seal, where retention runs, and where a writer that
                    // died asynchronously is noticed.
                    if let Err(e) = buffer.maintain() {
                        fatal(&e, &buffer_path);
                    }
                    shared_state.set_at_retention_bound(buffer.at_retention_bound());
                }
            }

            // A SIGHUP / ctrl-c capture, started here and finished on the
            // `capture_rx` arm above. The recording goes on running underneath
            // it — the state stays CAPTURING so a second signal still means
            // "exit when this finishes", but the loop is free the whole time.
            if !capturing {
                let state = STATE.load(Ordering::SeqCst);
                if state >= TERMINATING {
                    // Signalled twice before the capture even began: the second
                    // signal asked to stop, and there is nothing to wait for.
                    break;
                }
                if state == CAPTURING {
                    capturing = true;
                    info!("capture in progress; the recording continues");
                    let (buffer_path, output) = (buffer_path.clone(), output.clone());
                    let (gate, done) = (dump_gate.clone(), capture_tx.clone());
                    tokio::spawn(async move {
                        let _serialized = gate.lock().await;
                        let response = tokio::task::spawn_blocking(move || {
                            dump_to_file(&buffer_path, &output, &TimeRange::default())
                        })
                        .await
                        .unwrap_or_else(|e| {
                            DumpToFileResponse::error(format!("the capture task failed: {e}"))
                        });
                        let _ = done.send(response).await;
                    });
                }
            }
        }

        // A dump in flight at shutdown is finished, not abandoned: its caller
        // is still waiting on a reply, and the buffer it is reading lives in a
        // staging directory this function removes on the way out.
        if !dumps.is_empty() {
            info!("waiting for {} dump(s) in flight", dumps.len());
            while dumps.join_next().await.is_some() {}
        }
    });

    // Only reached on a clean exit; the buffer directory goes with it.
    drop(staging);
}

/// Write the buffer out to the configured output path.
///
/// This is the whole of what the ring's `perform_dump_to_file` did by walking
/// slots and running a msgpack→parquet conversion over them. It touches
/// neither the buffer nor the writer: `VACUUM INTO` copies a point-in-time
/// snapshot from its own connection while the recording continues.
fn dump_to_file(buffer_path: &Path, output: &Path, range: &TimeRange) -> DumpToFileResponse {
    match buffer::dump(buffer_path, output, range) {
        Ok(summary) => DumpToFileResponse::success(output.to_path_buf(), summary),
        Err(e) => DumpToFileResponse::error(e),
    }
}

/// Report a SIGHUP / ctrl-c capture. It is the only trace such a capture
/// leaves: there is no caller to answer, so a failure that is not logged here
/// is a failure nobody ever hears about.
fn log_capture(response: &DumpToFileResponse) {
    if let Some(error) = &response.error {
        error!("dump failed: {}", error);
    } else if let Some(summary) = &response.summary {
        // The span is what an operator actually wants to read back ("did I
        // catch the incident?"), so it leads; whole seconds because nanosecond
        // precision here is noise.
        let span = summary.retained().unwrap_or_default();
        info!(
            "capture complete: {} of metrics, {} rows across {} tables \
             ({} bytes) written to {}",
            humantime::format_duration(Duration::from_secs(span.as_secs())),
            summary.rows,
            summary.tables.len(),
            summary.bytes,
            response.path.display()
        );
    }
}

/// A buffer write that failed is not recoverable in place — but everything
/// committed before it is, and unlike the ring it is in a file anything can
/// open. Say where before exiting.
fn fatal(error: &str, buffer_path: &Path) -> ! {
    error!("the hindsight buffer failed: {error}");
    error!(
        "note: everything recorded so far is readable at {}",
        buffer_path.display()
    );
    std::process::exit(1);
}

fn wall_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// The recording's file-level metadata, matching what `rezolus record` writes
/// so a dump is indistinguishable from a recording to every consumer.
fn buffer_metadata(
    interval: Duration,
    systeminfo: &Option<String>,
    descriptions: &Option<String>,
) -> std::collections::BTreeMap<String, String> {
    let mut m = std::collections::BTreeMap::new();
    m.insert(
        "sampling_interval_ms".to_string(),
        interval.as_millis().to_string(),
    );
    m.insert("source".to_string(), "rezolus".to_string());
    if let Some(json) = systeminfo {
        m.insert("systeminfo".to_string(), json.clone());
    }
    if let Some(json) = descriptions {
        m.insert("descriptions".to_string(), json.clone());
    }
    m
}
