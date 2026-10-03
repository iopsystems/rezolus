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
             TRIGGERING A SNAPSHOT: send SIGHUP to write the buffer to a timestamped file beside\n\
             the output path (rezolus-20260915T204500Z.rez for output rezolus.rez) without\n\
             stopping the daemon. A SIGHUP during a capture is ignored. Optionally set\n\
             [general] listen to enable an HTTP endpoint for remote status/dump requests;\n\
             POST /dump/file writes the output path itself. Either way the recording keeps\n\
             running for the whole of the snapshot — a capture costs no samples, including\n\
             the samples taken while it is being written.\n\n\
             STOPPING: SIGTERM or SIGINT (what systemctl stop and ctrl-c send) captures the\n\
             buffer the same way, then exits with status 0, or 1 if that capture failed. A\n\
             stop during a SIGHUP capture exits when that capture completes. A second stop\n\
             exits at once with status 2, removing the buffer directory and abandoning the\n\
             capture in progress.\n\n\
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
/// configured lookback. On SIGHUP it writes the buffer to a timestamped file
/// beside the output path; on SIGTERM or SIGINT it does the same and exits.
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

    // Wakes the recording loop after a signal changes `signals::STATE`, so a
    // capture starts without waiting for a tick. The loop reads the state
    // itself; a full channel already holds a wake, so dropping another loses
    // nothing.
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel::<()>(1);
    listen_for_signals(&rt, signal_tx);

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
    // The buffered agent's version, not this binary's: hindsight and the agent
    // are separate processes and can be different builds. `/status` is the
    // structured answer; the root page is the fallback for agents older than
    // it. Same two-step as `recorder::fetch_agent_version`, done with the
    // blocking client hindsight already has here.
    let agent_status = fetch("/status").and_then(|body| {
        serde_json::from_str::<crate::agent::sampler_status::AgentStatus>(&body).ok()
    });
    let agent_version = agent_status
        .as_ref()
        .map(|s| s.version.clone())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            fetch("/")
                .as_deref()
                .and_then(crate::recorder::parse_root_version)
        });
    // No epoch from the root-page fallback: an agent old enough to lack
    // `/status` predates the epoch, and inventing one would claim a restart
    // boundary nobody observed.
    let agent_epoch = agent_status
        .as_ref()
        .map(|s| s.producer_epoch.clone())
        .filter(|e| !e.is_empty());

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
    // A `.dendro` output selects a dendro buffer, written through
    // metriken-archive; anything else is a `.rez` buffer, as before. Opt-in,
    // as for `record -o out.dendro`.
    let dendro = output.extension().is_some_and(|e| e == "dendro");
    if output.extension().is_some_and(|e| e == "parquet") {
        warn!(
            "{} will be written as a .rez archive, not parquet — hindsight snapshots \
             are `.rez` recordings since v3",
            output.display()
        );
    }

    let buffer_dir = config.general().buffer_dir();

    // Created if absent, so a hand-run daemon works without the operator
    // preparing anything. Under systemd `StateDirectory=rezolus` has already
    // made it, with the right ownership for `User=rezolus` — which is the case
    // that would otherwise fail, since a non-root service cannot create a
    // directory under `/var/lib` itself.
    if let Err(e) = std::fs::create_dir_all(&buffer_dir) {
        eprintln!(
            "could not create the buffer directory {}: {e}\n\
             Set `buffer_dir` in the config, or run under a unit with \
             `StateDirectory=rezolus`.",
            buffer_dir.display()
        );
        std::process::exit(1);
    }

    // The buffer lives in a private directory inside it, so its `-wal`/`-shm`
    // sidecars cannot collide with anything and the whole lot is removed
    // together when the daemon exits cleanly.
    let staging = match tempfile::TempDir::new_in(&buffer_dir) {
        Ok(t) => t,
        Err(error) => {
            eprintln!("could not open a buffer directory in: {buffer_dir:?}\n{error}");
            std::process::exit(1);
        }
    };
    signals::set_buffer_dir(staging.path());
    let buffer_path = staging.path().join(if dendro {
        "hindsight.dendro"
    } else {
        "hindsight.rez"
    });

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
        metadata: buffer_metadata(
            interval_dur,
            &agent_systeminfo,
            &agent_descriptions,
            &agent_version,
            &agent_epoch,
        ),
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

    let created = if dendro {
        HindsightBuffer::create_dendro(&buffer_path, seed, lookback, policy)
    } else {
        HindsightBuffer::create(&buffer_path, seed, lookback, policy)
    };
    let mut buffer = match created {
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

    let capture_failed = rt.block_on(async move {
        // The stop's capture failed: exit 1 rather than 0, so a supervisor
        // can tell the buffer was not saved.
        let mut capture_failed = false;
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
        // The signal-triggered capture, which has no caller to reply to: it
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
                    // Unless a stop was asked for, back to RUNNING BEFORE the
                    // log line, so a signal sent on seeing that line is acted
                    // on as a new request rather than as one made during the
                    // capture. A compare-and-swap: the signal task runs on
                    // another thread, and a stop it stored since must not be
                    // overwritten.
                    let terminating = signals::STATE
                        .compare_exchange(
                            signals::CAPTURING,
                            signals::RUNNING,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_err();
                    if terminating && response.error.is_some() {
                        capture_failed = true;
                    }
                    log_capture(&response);
                    if terminating {
                        break;
                    }
                }

                // A signal changed the signal state; the check below acts on
                // it.
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

            // A signal-triggered capture (SIGHUP, or the capture a stop
            // takes), started here and finished on the `capture_rx` arm above.
            // The recording goes on running underneath it, and the loop is
            // free the whole time. A SIGHUP during it is ignored; a stop
            // during it exits when it completes.
            if !capturing {
                let state = signals::STATE.load(Ordering::SeqCst);
                if state != signals::RUNNING {
                    // A capture was asked for and has not started. If a stop
                    // was asked for too (STOPPING, or TERMINATING when it came
                    // after a SIGHUP), this capture is the last: the loop exits
                    // when it completes.
                    if state == signals::STOPPING {
                        let _ = signals::STATE.compare_exchange(
                            signals::STOPPING,
                            signals::TERMINATING,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        );
                    }
                    capturing = true;
                    info!("capture in progress; the recording continues");
                    // NOT `output`. An HTTP dump writes there because its
                    // caller asked for exactly that path; a signal-triggered
                    // capture has no caller and no such instruction, and
                    // `output` is where an operator's deliberate captures
                    // live. Writing there on every stop would mean restarting
                    // the service overwrites the incident somebody saved —
                    // destroying data they chose to keep, which is worse than
                    // the window this capture exists to preserve.
                    let output = shutdown_capture_path(&output, wall_ns());
                    let buffer_path = buffer_path.clone();
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
        capture_failed
    });

    // Reached once the loop has stopped. Dropping `staging` deletes the buffer
    // directory, and the log drain flushes; both have to happen before `exit`,
    // which runs no destructors.
    drop(staging);
    if capture_failed {
        drop(_log_drain);
        std::process::exit(1);
    }
}

/// Hindsight's signal state. Separate from `crate::STATE`, which the recorder
/// uses.
mod signals {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    /// The buffer directory, for a forced exit to remove.
    static BUFFER_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

    pub fn set_buffer_dir(dir: &Path) {
        *BUFFER_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
    }

    /// Remove the buffer directory, for an exit that skips destructors.
    pub fn remove_buffer_dir() {
        if let Some(dir) = BUFFER_DIR.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// What signals have asked of the daemon. Read and written by the signal
    /// task and the recording loop.
    pub static STATE: AtomicUsize = AtomicUsize::new(RUNNING);

    /// Recording, with no capture asked for.
    pub const RUNNING: usize = 0;
    /// A capture was asked for (SIGHUP) or is in flight; recording continues
    /// after it.
    pub const CAPTURING: usize = 1;
    /// A stop was asked for while a capture was asked for or in flight: exit
    /// once that capture completes.
    pub const TERMINATING: usize = 2;
    /// A stop was asked for with no capture in flight: capture, then exit.
    pub const STOPPING: usize = 3;
}

/// What a signal asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    /// SIGHUP: capture the buffer and keep recording.
    Capture,
    /// SIGTERM or SIGINT: capture the buffer, then exit.
    Stop,
}

/// Apply a signal to [`signals::STATE`]. A SIGHUP while a capture is pending
/// or in progress is ignored. A stop while a capture is pending or in progress
/// exits once that capture completes, and a second stop exits at once with
/// status 2. The state changes by compare-and-swap, because the recording loop
/// changes it from another thread.
fn on_signal(request: Request) {
    let mut from = signals::STATE.load(Ordering::SeqCst);
    let applied = loop {
        let to = match (request, from) {
            (Request::Capture, signals::RUNNING) => signals::CAPTURING,
            (Request::Stop, signals::RUNNING) => signals::STOPPING,
            (Request::Stop, signals::CAPTURING) => signals::TERMINATING,
            _ => break false,
        };
        match signals::STATE.compare_exchange(from, to, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => break true,
            Err(now) => from = now,
        }
    };
    match (request, applied, from) {
        (Request::Capture, true, _) => {
            info!("SIGHUP: capturing the buffer; the recording continues")
        }
        (Request::Capture, false, _) => {
            info!("SIGHUP ignored: a capture is pending or in progress")
        }
        (Request::Stop, true, signals::RUNNING) => {
            info!("stop requested: capturing the buffer, then exiting")
        }
        (Request::Stop, true, _) => {
            info!("stop requested: exiting once the capture in progress completes")
        }
        (Request::Stop, false, _) => {
            // `exit` runs no destructors and the log drain may not flush, so
            // the buffer directory is removed here and the line goes straight
            // to stderr.
            eprintln!("second stop requested: exiting now");
            signals::remove_buffer_dir();
            std::process::exit(2);
        }
    }
}

/// Route SIGHUP to a capture and SIGTERM and SIGINT to a stop, waking the
/// recording loop through `wake` after each.
fn listen_for_signals(rt: &tokio::runtime::Runtime, wake: tokio::sync::mpsc::Sender<()>) {
    use tokio::signal::unix::{signal, SignalKind};
    let _guard = rt.enter();
    let listen = |kind: SignalKind| {
        signal(kind).unwrap_or_else(|e| {
            error!("could not listen for signals: {e}");
            std::process::exit(1);
        })
    };
    let (mut hup, mut term, mut int) = (
        listen(SignalKind::hangup()),
        listen(SignalKind::terminate()),
        listen(SignalKind::interrupt()),
    );
    rt.spawn(async move {
        loop {
            let request = tokio::select! {
                _ = hup.recv() => Request::Capture,
                _ = term.recv() => Request::Stop,
                _ = int.recv() => Request::Stop,
            };
            on_signal(request);
            let _ = wake.try_send(());
        }
    });
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

/// Where a signal-triggered capture goes: `output`'s directory and stem, a
/// UTC timestamp, and `output`'s extension.
///
/// `/var/lib/rezolus/rezolus.rez` becomes
/// `/var/lib/rezolus/rezolus-20260915T204500Z.rez`.
///
/// Timestamped rather than a fixed second name so that successive restarts do
/// not overwrite each other either — two stops a minute apart are two
/// different windows, and the second is not more interesting than the first.
/// It does mean these accumulate; retention for them is deliberately not
/// handled here, because a file an operator may be about to read is not
/// something a daemon should delete on its own schedule.
fn shutdown_capture_path(output: &Path, now_ns: u64) -> PathBuf {
    let stamp = chrono::DateTime::from_timestamp_nanos(now_ns as i64)
        .format("%Y%m%dT%H%M%SZ")
        .to_string();
    let stem = output
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rezolus".to_string());
    let name = match output.extension() {
        Some(ext) => format!("{stem}-{stamp}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{stamp}"),
    };
    output.with_file_name(name)
}

/// Report a signal-triggered capture. It is the only trace such a capture
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
    version: &Option<String>,
    producer_epoch: &Option<String>,
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
    if let Some(v) = version {
        m.insert(crate::parquet_metadata::KEY_VERSION.to_string(), v.clone());
    }
    if let Some(e) = producer_epoch {
        m.insert(
            crate::parquet_metadata::KEY_PRODUCER_EPOCH.to_string(),
            e.clone(),
        );
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signal-triggered capture must never land on `output`: that is where
    /// an operator's deliberate `POST /dump/file` captures go, and a service
    /// restart would otherwise overwrite an incident somebody saved.
    #[test]
    fn a_shutdown_capture_does_not_land_on_the_output_path() {
        let out = Path::new("/var/lib/rezolus/rezolus.rez");
        let got = shutdown_capture_path(out, 1_789_425_944_000_000_000);
        assert_ne!(got, out.to_path_buf(), "must not clobber the output path");
        assert_eq!(
            got,
            Path::new("/var/lib/rezolus/rezolus-20260914T224544Z.rez"),
            "same directory and extension, stem stamped with the capture time"
        );
    }

    /// Two stops close together are two different windows; neither is more
    /// interesting than the other, so neither may overwrite the other.
    #[test]
    fn two_shutdown_captures_do_not_collide() {
        let out = Path::new("/var/lib/rezolus/rezolus.rez");
        let a = shutdown_capture_path(out, 1_789_425_944_000_000_000);
        let b = shutdown_capture_path(out, 1_789_425_999_000_000_000);
        assert_ne!(a, b);
    }

    /// An output path with no extension still produces a distinct name rather
    /// than panicking or returning the input.
    #[test]
    fn a_shutdown_capture_handles_an_extensionless_output() {
        let out = Path::new("/var/lib/rezolus/buffer");
        let got = shutdown_capture_path(out, 1_789_425_944_000_000_000);
        assert_ne!(got, out.to_path_buf());
        assert_eq!(got, Path::new("/var/lib/rezolus/buffer-20260914T224544Z"));
    }

    /// The buffer must be indistinguishable from a `rezolus record` capture to
    /// every consumer, and that now includes carrying the agent's version
    /// (issue #1195). Hindsight and the agent are separate processes, so the
    /// version written is the one fetched from the agent, never this binary's.
    #[test]
    fn buffer_metadata_carries_the_agent_version_when_the_agent_reported_one() {
        let m = buffer_metadata(
            Duration::from_secs(1),
            &None,
            &None,
            &Some("5.19.2".to_string()),
            &None,
        );
        assert_eq!(
            m.get(crate::parquet_metadata::KEY_VERSION)
                .map(String::as_str),
            Some("5.19.2")
        );

        // Absent, not empty: an empty value renders as the bare
        // `Rezolus Version:` line this change exists to remove.
        let m = buffer_metadata(Duration::from_secs(1), &None, &None, &None, &None);
        assert!(!m.contains_key(crate::parquet_metadata::KEY_VERSION));
    }
}
