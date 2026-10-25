//! Action handlers — endpoints that mutate `AppState` (uploads, attach
//! and detach, live agent connect and reset, save). Live-mode recording is
//! in `live.rs`.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Json, Response};
use http::{header, StatusCode};
use reqwest::{Client, Url};
use tracing::{error, info, warn};

use metriken_query::MetricsSource;

use super::capture_registry::CaptureId;
use super::metadata::{compute_file_checksum, regenerate_dashboards};
use super::report_save;
use super::routes::on_query_slot;
use super::state::{ApiResponse, AppState, LazySectionStore};

// ── Agent info ────────────────────────────────────────────────────────

/// Fetch the agent banner (`source version`) and `/systeminfo`. Used by
/// CLI startup and the runtime `/api/v1/connect` handler.
pub async fn fetch_agent_info(client: &Client, url: &Url) -> Result<AgentInfo, String> {
    let resp = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| format!("failed to connect to agent at {url}: {e}"))?;
    let banner = resp.text().await.unwrap_or_default();
    let first_line = banner.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    let (source, version) = match parts.as_slice() {
        [name, ver, ..] => (name.to_string(), ver.to_string()),
        _ => {
            warn!("unexpected agent banner: {first_line:?}");
            ("rezolus".to_string(), String::new())
        }
    };

    let mut info_url = url.clone();
    info_url.set_path("/systeminfo");
    let sysinfo = match client.get(info_url).send().await {
        Ok(r) if r.status().is_success() => r.text().await.ok(),
        _ => None,
    };

    Ok(AgentInfo {
        source,
        version,
        sysinfo,
    })
}

#[derive(Clone)]
pub struct AgentInfo {
    pub source: String,
    pub version: String,
    pub sysinfo: Option<String>,
}

// ── Upload / load_url ─────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct LoadUrlBody {
    url: String,
    #[serde(default)]
    filename: Option<String>,
}

/// Fetch a remote parquet on the browser's behalf and ingest it. Refuses
/// every request when `--proxy-allow` was not set or when the host
/// doesn't match any allowlist pattern.
pub async fn load_url(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoadUrlBody>,
) -> Json<ApiResponse<serde_json::Value>> {
    if state.live.load(Ordering::Relaxed) {
        return ApiResponse::err("load_url is only available in file mode", "bad_request");
    }
    let Some(client) = state.proxy.client.as_ref() else {
        return ApiResponse::err("url loading is disabled", "forbidden");
    };

    let target = match Url::parse(&body.url) {
        Ok(u) => u,
        Err(e) => return ApiResponse::err(format!("invalid url: {e}"), "bad_request"),
    };
    if !matches!(target.scheme(), "http" | "https") {
        return ApiResponse::err("url scheme must be http or https", "bad_request");
    }
    let Some(host) = target.host_str().map(str::to_string) else {
        return ApiResponse::err("url is missing a host", "bad_request");
    };
    if !state.proxy.allow.allows(&host) {
        return ApiResponse::err(
            format!("host {host} not in --proxy-allow list"),
            "forbidden",
        );
    }

    let upstream = match client.get(target.clone()).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("load_url fetch failed for {target}: {e}");
            return ApiResponse::err(format!("upstream fetch failed: {e}"), "upstream_error");
        }
    };
    if !upstream.status().is_success() {
        return ApiResponse::err(
            format!("upstream returned {}", upstream.status()),
            "upstream_error",
        );
    }
    let bytes = match upstream.bytes().await {
        Ok(b) => b,
        Err(e) => return ApiResponse::err(format!("upstream read failed: {e}"), "upstream_error"),
    };

    let filename = body.filename.unwrap_or_else(|| {
        target
            .path_segments()
            .and_then(|mut s| s.rfind(|seg| !seg.is_empty()))
            .map(ToString::to_string)
            .unwrap_or_else(|| "remote.dendro".to_string())
    });
    // Staging, opening and checksumming the file and building the
    // dashboards read the whole file, so they run in a query slot.
    on_query_slot(&state, move |state| {
        // Checked again here: a live connect can finish during the wait.
        if state.live.load(Ordering::Relaxed) {
            return ApiResponse::err("load_url is only available in file mode", "bad_request");
        }
        ingest_upload(state, &bytes, filename)
    })
    .await
}

/// Upload and load a recording into file-mode viewer state.
pub async fn upload_parquet(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Json<ApiResponse<serde_json::Value>> {
    if state.live.load(Ordering::Relaxed) {
        return ApiResponse::err("upload is only available in file mode", "bad_request");
    }
    if body.is_empty() {
        return ApiResponse::err("missing recording bytes", "bad_request");
    }

    let filename = filename_header(&headers).unwrap_or_else(|| "upload.dendro".to_string());
    // Storing, opening and checksumming the upload and building the
    // dashboards read the whole file, so they run in a query slot.
    on_query_slot(&state, move |state| ingest_upload(state, &body, filename)).await
}

/// Store an upload and load it as the baseline: a dendro archive, by its
/// content. Any other file is refused, one an earlier release wrote with the
/// `recording upgrade` command that converts it.
fn ingest_upload(
    state: &AppState,
    body: &[u8],
    filename: String,
) -> Json<ApiResponse<serde_json::Value>> {
    // Checked again here: a live connect can finish during the slot wait.
    if state.live.load(Ordering::Relaxed) {
        return ApiResponse::err("upload is only available in file mode", "bad_request");
    }
    let container = ::rez::container::Container::of_bytes(body);
    if container != ::rez::container::Container::Dendro {
        return ApiResponse::err(
            ::rez::container::not_dendro(&filename, container, env!("CARGO_BIN_NAME")),
            "invalid_recording",
        );
    }
    let temp_path = baseline_temp_path();
    if let Err(e) = std::fs::write(&temp_path, body) {
        return ApiResponse::err(format!("failed to store upload: {e}"), "io_error");
    }
    ingest_rez_from_path(state, temp_path, filename)
}

/// Runtime-upload version of `init_file_mode_rez` (mod.rs): load a dendro
/// archive as one `RezReader` per recording, wiring a 2-recording archive
/// onto the baseline/experiment slots (>2 shows the first two). Returns an
/// HTTP envelope.
fn ingest_rez_from_path(
    state: &AppState,
    rez_path: PathBuf,
    display_filename: String,
) -> Json<ApiResponse<serde_json::Value>> {
    use metriken_query::MetricsSource;

    let readers =
        match crate::rez_reader::RezReader::open_recordings(&rez_path, Arc::clone(&state.pool)) {
            Ok(r) if !r.is_empty() => r,
            Ok(_) => {
                let _ = std::fs::remove_file(&rez_path);
                return ApiResponse::err("the archive holds no recordings", "invalid_recording");
            }
            Err(e) => {
                let _ = std::fs::remove_file(&rez_path);
                return ApiResponse::err(
                    format!("failed to read the archive: {e}"),
                    "invalid_recording",
                );
            }
        };

    // An archive of one run opens as one combined view, as it does from the
    // command line (`super::init_file_mode_combined`); an upload has no
    // --baseline/--experiment to ask for a comparison.
    if super::opens_combined(&readers, false) {
        let combined = super::combine_recordings(readers);
        let markers = combined.markers.clone();
        let file_meta = super::file_meta_json(&combined.reader)
            .map(|meta| ::dashboard::combined::with_report_markers(&meta, &markers));
        let selection =
            ::dashboard::combined::marker(&markers, crate::parquet_metadata::KEY_SELECTION);
        let report_marker =
            ::dashboard::combined::marker(&markers, crate::parquet_metadata::KEY_REPORT);
        let reader = Arc::new(combined.reader);
        state.replace_baseline(Arc::clone(&reader) as Arc<dyn MetricsSource>);
        *state.combined_catalog.write() =
            Some(Arc::new(::dashboard::combined::FixedCatalog(reader)));
        *state.parquet_path.write() = Some(rez_path.clone());
        *state.baseline_name.write() = Some(display_filename.clone());
        *state.baseline_recordings.write() = combined.labels.len();
        *state.experiment_parquet_path.write() = None;
        *state.cli_experiment_path.write() = None;
        *state.selection.write() = selection;
        *state.trimmed_report_marker.write() = report_marker;
        state.captures.set_baseline_systeminfo(combined.systeminfo);
        state.captures.set_baseline_file_metadata(file_meta);
        *state.file_checksum.write() = compute_file_checksum(&rez_path);
        state.captures.set_baseline_alias(None);
        state.captures.detach_experiment();
        *state.members.write() = combined.members;
        regenerate_dashboards(state);
        return ApiResponse::ok(serde_json::json!({ "filename": display_filename }));
    }

    fn describe(reader: &crate::rez_reader::RezReader) -> (Option<String>, Option<String>) {
        use metriken_query::MetricsSource;
        let systeminfo = reader.metadata_get("systeminfo");
        let file_meta = {
            let mut map = serde_json::Map::new();
            for (k, v) in reader.file_metadata() {
                let jv = serde_json::from_str(&v).unwrap_or(serde_json::Value::String(v.clone()));
                map.insert(k, jv);
            }
            serde_json::to_string(&serde_json::Value::Object(map)).ok()
        };
        (systeminfo, file_meta)
    }
    // Same rule as the file-mode path: the alias must come from a label that
    // actually differs across these recordings, or a same-host A/B names both
    // arms identically.
    let alias_key = crate::viewer::discriminating_alias_key(
        &readers.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
    );
    let alias_of =
        |labels: &std::collections::BTreeMap<String, String>, fallback: &str| -> String {
            alias_key
                .as_ref()
                .and_then(|k| labels.get(k))
                .cloned()
                .unwrap_or_else(|| fallback.to_string())
        };

    // The arms say which recording is which (`dashboard::combined::arm_slots`).
    let labels: Vec<_> = readers.iter().map(|(l, _)| l.clone()).collect();
    let slots = match ::dashboard::combined::arm_slots(&labels) {
        Ok(slots) => slots,
        Err(why) => {
            let _ = std::fs::remove_file(&rez_path);
            return ApiResponse::err(
                format!("{why}; open it with `rezolus view FILE --baseline k=v --experiment k=v`"),
                "invalid_recording",
            );
        }
    };
    if let Some(note) = &slots.note {
        tracing::info!("{note}");
    }
    // The slots, then every other recording beside the comparison, each in
    // its own sections (`super::init_file_mode_rez` does the same).
    let mut picked: Vec<Option<_>> = readers.into_iter().map(Some).collect();
    let (b_labels, b_reader) = picked[slots.baseline].take().expect("in range");
    let experiment = slots
        .experiment
        .map(|e| picked[e].take().expect("in range"));
    let positions: Vec<usize> = (0..picked.len()).filter(|&i| picked[i].is_some()).collect();
    let rest: Vec<_> = picked.into_iter().flatten().collect();
    let names = ::dashboard::dashboard::beside_names(
        &rest.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
        &positions,
    );
    let beside: Vec<::dashboard::dashboard::Beside> = rest
        .into_iter()
        .zip(names)
        .map(
            |((_, reader), (key, name))| ::dashboard::dashboard::Beside {
                key,
                name,
                data: Arc::new(reader) as Arc<dyn MetricsSource>,
            },
        )
        .collect();

    let (b_systeminfo, b_file_meta) = describe(&b_reader);
    let b_alias = alias_of(&b_labels, "baseline");
    // A report's selection, marker and events: the baseline's, else those
    // of the first recording with a selection (as `super::init_file_mode_rez`
    // reads them).
    let markers = {
        let shown: Vec<&dyn MetricsSource> = std::iter::once(&b_reader as &dyn MetricsSource)
            .chain(experiment.iter().map(|(_, r)| r as &dyn MetricsSource))
            .chain(beside.iter().map(|b| b.data.as_ref()))
            .collect();
        ::dashboard::combined::report_markers(&shown)
    };
    let b_file_meta =
        b_file_meta.map(|meta| ::dashboard::combined::with_report_markers(&meta, &markers));
    *state.selection.write() =
        ::dashboard::combined::marker(&markers, crate::parquet_metadata::KEY_SELECTION);
    *state.trimmed_report_marker.write() =
        ::dashboard::combined::marker(&markers, crate::parquet_metadata::KEY_REPORT);
    let primary = Arc::new(b_reader) as Arc<dyn MetricsSource>;
    let baseline: Arc<dyn MetricsSource> = if beside.is_empty() {
        primary
    } else {
        Arc::new(::dashboard::combined::Routed::new(
            primary,
            beside
                .iter()
                .map(|b| (b.key.clone(), Arc::clone(&b.data)))
                .collect(),
        ))
    };
    state.replace_baseline(baseline);
    let b_systeminfo =
        ::dashboard::dashboard::with_beside_systeminfo(b_systeminfo, &b_alias, &beside);
    let b_file_meta =
        b_file_meta.map(|meta| ::dashboard::dashboard::with_beside_metadata(&meta, &beside));
    *state.beside.write() = beside;
    *state.anchor_labels.write() = Some(b_labels.clone());
    *state.parquet_path.write() = Some(rez_path.clone());
    *state.baseline_name.write() = Some(display_filename.clone());
    *state.baseline_recordings.write() = labels.len();
    *state.experiment_parquet_path.write() = None;
    *state.cli_experiment_path.write() = None;
    state.captures.set_baseline_systeminfo(b_systeminfo);
    state.captures.set_baseline_file_metadata(b_file_meta);
    *state.file_checksum.write() = compute_file_checksum(&rez_path);
    state.captures.set_baseline_alias(Some(b_alias));

    match experiment {
        Some((e_labels, e_reader)) => {
            let (e_systeminfo, e_file_meta) = describe(&e_reader);
            let e_alias = alias_of(&e_labels, "experiment");
            state.captures.attach_experiment(
                Arc::new(e_reader) as Arc<dyn MetricsSource>,
                e_systeminfo,
                e_file_meta,
                Some(e_alias),
            );
        }
        None => state.captures.detach_experiment(),
    }

    regenerate_dashboards(state);
    // Keep the uploaded archive on disk: parquet_path references it.
    ApiResponse::ok(serde_json::json!({ "filename": display_filename }))
}

pub fn baseline_temp_path() -> PathBuf {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("rezolus-viewer-{}-{}", std::process::id(), suffix))
}

fn filename_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-rezolus-filename")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string)
}

// ── Experiment attach / detach ────────────────────────────────────────

/// Attach a one-recording dendro archive as the experiment of an A/B
/// comparison, beside a one-recording baseline. Body is the archive's bytes.
/// Returns 409 if an experiment is already attached, and 400 with the
/// commands that compare two recordings when either file holds several.
pub async fn attach_experiment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if state.captures.experiment_attached() {
        return (
            StatusCode::CONFLICT,
            "experiment already attached; DELETE first",
        )
            .into_response();
    }
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing recording bytes").into_response();
    }

    let filename = filename_header(&headers).unwrap_or_else(|| "experiment.dendro".to_string());
    // Writing the upload, opening it and rebuilding the dashboards (which run
    // KPI validation queries) are blocking file reads, so they run in a query
    // slot.
    on_query_slot(&state, move |state| attach_upload(state, &body, filename)).await
}

/// Store an uploaded experiment archive and attach it.
fn attach_upload(state: &AppState, body: &[u8], filename: String) -> Response {
    // Checked again here: another attach can finish during the slot wait, and
    // both would write the same temp file.
    if state.captures.experiment_attached() {
        return (
            StatusCode::CONFLICT,
            "experiment already attached; DELETE first",
        )
            .into_response();
    }
    let container = ::rez::container::Container::of_bytes(body);
    if container != ::rez::container::Container::Dendro {
        return (
            StatusCode::BAD_REQUEST,
            ::rez::container::not_dendro(&filename, container, env!("CARGO_BIN_NAME")),
        )
            .into_response();
    }
    let baseline = state
        .baseline_name
        .read()
        .clone()
        .unwrap_or_else(|| "BASELINE.dendro".to_string());
    // The baseline of a two-file comparison is one recording too.
    let baseline_recordings = *state.baseline_recordings.read();
    if baseline_recordings != 1 {
        return (
            StatusCode::BAD_REQUEST,
            ::rez::container::one_recording_hint(
                env!("CARGO_BIN_NAME"),
                &baseline,
                "the baseline",
                baseline_recordings,
                &baseline,
                &filename,
            ),
        )
            .into_response();
    }
    let temp_path =
        std::env::temp_dir().join(format!("rezolus-experiment-{}.dendro", std::process::id()));
    if let Err(e) = std::fs::write(&temp_path, body) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to store upload: {e}"),
        )
            .into_response();
    }

    let (exp_reader, sysinfo, file_meta) =
        match super::open_experiment(&temp_path, &filename, &baseline, Arc::clone(&state.pool)) {
            Ok(opened) => opened,
            Err(e) => {
                let _ = std::fs::remove_file(&temp_path);
                // The hint names the staged file; the caller knows it by the
                // name it uploaded.
                let e = e.replace(&temp_path.display().to_string(), &filename);
                return (StatusCode::BAD_REQUEST, e).into_response();
            }
        };
    let exp_reader = exp_reader.with_filename(filename);

    // HTTP-attached experiments don't carry an alias today; the
    // parameter is here so a future `x-rezolus-alias` header can thread
    // one through without further signature changes.
    state.unfollow_capture(super::capture_registry::EXPERIMENT_ID);
    state.captures.attach_experiment(
        Arc::new(exp_reader) as Arc<dyn MetricsSource>,
        sysinfo.clone(),
        file_meta,
        None,
    );
    *state.experiment_parquet_path.write() = Some(temp_path);

    regenerate_dashboards(state);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        sysinfo.unwrap_or_else(|| "{}".into()),
    )
        .into_response()
}

/// Detach the currently attached experiment (if any) and clean up its temp file.
///
/// All of it runs in a query slot, since rebuilding the dashboards runs KPI
/// validation queries; a request dropped while waiting for the slot changes
/// nothing.
pub async fn detach_experiment(State(state): State<Arc<AppState>>) -> Response {
    on_query_slot(&state, |state| {
        state.unfollow_capture(super::capture_registry::EXPERIMENT_ID);
        state.captures.detach_experiment();
        if let Some(path) = state.experiment_parquet_path.write().take() {
            let _ = std::fs::remove_file(&path);
        }
        // Clear the CLI-supplied experiment path too, so Save as Report
        // (`resolve_experiment_parquet_path`) does not pair a detached file
        // with the baseline. Only the path reference is dropped — the user's
        // parquet on disk is left alone. The regen below reads templates from
        // the attached captures, so the detached one drops out with it.
        state.cli_experiment_path.write().take();
        regenerate_dashboards(state);
    })
    .await;
    StatusCode::OK.into_response()
}

// ── Live agent connect / reset ────────────────────────────────────────

/// The `live` flag, claimed for one connect. Released when dropped unless
/// [`keep`](Self::keep) was called.
struct LiveClaim<'a>(&'a std::sync::atomic::AtomicBool, bool);

impl<'a> LiveClaim<'a> {
    /// Set the flag, or `None` if it was already set.
    fn take(flag: &'a std::sync::atomic::AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(flag, false))
    }

    /// Leave the flag set.
    fn keep(mut self) {
        self.1 = true;
    }
}

impl Drop for LiveClaim<'_> {
    fn drop(&mut self) {
        if !self.1 {
            self.0.store(false, Ordering::Release);
        }
    }
}

/// Connect to a live Rezolus agent at runtime.
pub async fn connect_agent(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Json<ApiResponse<serde_json::Value>> {
    // Claimed before the awaits, so two connects racing cannot both start a
    // session; released on every error return.
    let Some(claim) = LiveClaim::take(&state.live) else {
        return ApiResponse::err("already connected to a live agent", "bad_request");
    };

    let url_str = match std::str::from_utf8(&body) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return ApiResponse::err("invalid UTF-8 in URL", "bad_request"),
    };
    let url: Url = match url_str.parse() {
        Ok(u) => u,
        Err(e) => return ApiResponse::err(format!("invalid URL: {e}"), "bad_request"),
    };

    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            return ApiResponse::err(
                format!("failed to create HTTP client: {e}"),
                "internal_error",
            );
        }
    };

    let info = match fetch_agent_info(&client, &url).await {
        Ok(i) => i,
        Err(e) => return ApiResponse::err(e, "connection_error"),
    };

    let (source, version, sysinfo) = (
        info.source.clone(),
        info.version.clone(),
        info.sysinfo.clone(),
    );
    let session =
        match super::live::LiveSession::start(&client, &url, info, Arc::clone(&state.pool)).await {
            Ok(session) => session,
            Err(e) => return ApiResponse::err(e, "connection_error"),
        };
    let context = super::metadata::live_dashboard_context(
        session.reader().as_ref(),
        state.template_preference,
    );

    state.install_live(session);
    *state.sections.write() = LazySectionStore::new(context);
    state.captures.set_baseline_systeminfo(sysinfo);
    claim.keep();
    let info = AgentInfo {
        source,
        version,
        sysinfo: None,
    };

    info!(
        "Connected to {source} {version} at {url}",
        source = info.source,
        version = info.version
    );

    ApiResponse::ok(serde_json::json!({
        "source": info.source,
        "version": info.version,
        "url": url.to_string(),
    }))
}

/// Start a fresh live session against the same agent and install it; the
/// old session is dropped.
pub async fn reset_tsdb(
    State(state): State<Arc<AppState>>,
) -> Json<ApiResponse<serde_json::Value>> {
    if !state.live.load(Ordering::Relaxed) {
        return ApiResponse::err("reset is only available in live mode", "bad_request");
    }

    let client = match Client::builder().http1_only().build() {
        Ok(c) => c,
        Err(e) => {
            return ApiResponse::err(
                format!("failed to create HTTP client: {e}"),
                "internal_error",
            );
        }
    };
    // The old session is dropped once the new one is in place. That stops its
    // recording; its archive is deleted once its recording thread and any
    // save in progress release it.
    let target = state.live_session.lock().as_ref().map(|s| s.target());
    let Some((url, info)) = target else {
        return ApiResponse::err("no live agent is being recorded", "bad_request");
    };
    let session =
        match super::live::LiveSession::start(&client, &url, info, Arc::clone(&state.pool)).await {
            Ok(session) => session,
            Err(e) => return ApiResponse::err(e, "connection_error"),
        };
    state.install_live(session);
    info!("TSDB reset by user");
    ApiResponse::ok(serde_json::json!({ "ok": true }))
}

// ── Save ──────────────────────────────────────────────────────────────

fn parquet_attachment(filename: &str, body: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(body))
        .unwrap()
}

fn server_error(msg: impl Into<String>) -> Response {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from(msg.into()))
        .unwrap()
}

/// Save the live recording: a sealed copy of its archive, as a `.dendro`.
pub async fn save_capture(State(state): State<Arc<AppState>>) -> Response {
    let archive = state.live_session.lock().as_ref().map(|s| s.archive());
    let Some((path, hold)) = archive else {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap();
    };
    let result = tokio::task::spawn_blocking(move || {
        let _hold = hold;
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let dest = dir.path().join("capture.dendro");
        crate::hindsight::buffer::dump(
            &path,
            &dest,
            &crate::hindsight::state::TimeRange::new(None, None),
        )?;
        std::fs::read(&dest).map_err(|e| e.to_string())
    })
    .await;
    finalize_attachment(result, "rezolus-capture.dendro", parquet_attachment)
}

/// Save as Report: a trimmed `.dendro` of the loaded archive (of both
/// archives, arm-labelled, for a two-file comparison) carrying the saved
/// selection, streamed back. In live mode `parquet_path` is the live
/// archive.
///
/// The report is built in a query slot: resolving the kept columns builds a
/// reader for each table a saved query touches, and the build reads the
/// recording. What the report is built from is read before the slot wait,
/// together with `parquet_path`, so a load during the wait does not pair one
/// recording's path with another's data.
pub async fn save_with_selection(State(state): State<Arc<AppState>>, body: String) -> Response {
    // In live mode `parquet_path` is the live archive: hold its directory
    // until the report is built, so a reset meanwhile does not delete it.
    // Both are read under the session lock, which `install_live` holds while
    // it changes them.
    let (parquet_path, live_hold, sources) = {
        let session = state.live_session.lock();
        (
            state.parquet_path.read().clone(),
            session.as_ref().map(|s| s.archive().1),
            ReportSources::read(&state),
        )
    };
    // Live mode sets `parquet_path` to its archive; with no file and no live
    // agent there is nothing to save.
    let Some(path) = parquet_path else {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap();
    };
    let selection_json = body;
    let payload: report_save::ReportPayload = match serde_json::from_str(&selection_json) {
        Ok(p) => p,
        Err(e) => {
            return ApiResponse::<()>::err(format!("invalid selection payload: {e}"), "bad_data")
                .into_response();
        }
    };
    let range = match payload.time_range() {
        Ok(r) => r,
        Err(e) => return ApiResponse::<()>::err(e, "bad_data").into_response(),
    };
    on_query_slot(&state, move |_| {
        let _hold = live_hold;
        let (result, filename) = build_report(&sources, &path, &payload, &selection_json, range);
        finalize_build(result, filename, parquet_attachment)
    })
    .await
}

/// The data a report is built from, read from the state at one time.
struct ReportSources {
    /// Every attached capture, anchor first.
    captures: Vec<Arc<dyn MetricsSource>>,
    /// The experiment parquet's path and data, when one is attached.
    experiment: Option<(PathBuf, Arc<dyn MetricsSource>)>,
    /// The labels of the archive recording the baseline reads
    /// (`AppState::anchor_labels`): the report's markers go on it.
    anchor: Option<std::collections::BTreeMap<String, String>>,
}

impl ReportSources {
    fn read(state: &AppState) -> Self {
        ReportSources {
            captures: state
                .captures
                .capture_ids()
                .iter()
                .filter_map(|id| state.captures.get_by_id(id))
                .collect(),
            experiment: state
                .resolve_experiment_parquet_path()
                .zip(state.captures.get(CaptureId::Experiment)),
            anchor: state.anchor_labels.read().clone(),
        }
    }
}

/// Build the report for the recording at `path` and name its download: a
/// `.dendro`, whatever the source (`viewer::report_save::build_report`).
fn build_report(
    sources: &ReportSources,
    path: &std::path::Path,
    payload: &report_save::ReportPayload,
    selection_json: &str,
    range: Option<::report_save::TimeRange>,
) -> (Result<Vec<u8>, String>, &'static str) {
    let trim_columns = payload.trim_columns;
    let events_json = if payload.events.is_empty() {
        None
    } else {
        serde_json::to_string(&serde_json::json!({ "events": &payload.events })).ok()
    };

    // Union the kept columns across every attached capture — the anchor as
    // baseline, the rest as experiment — so an A/B keeps both sides' queried
    // metrics. A slightly looser set than a per-recording trim, but never
    // lossy.
    let keep: Option<std::collections::BTreeSet<String>> = trim_columns.then(|| {
        let mut set = std::collections::BTreeSet::new();
        for (i, source) in sources.captures.iter().enumerate() {
            let side = if i == 0 {
                ::report_save::Side::Baseline
            } else {
                ::report_save::Side::Experiment
            };
            set.extend(::report_save::resolve_kept_columns(
                payload,
                source.as_ref(),
                side,
            ));
        }
        set
    });

    // One archive, or a two-file comparison: the baseline's file and the
    // experiment's, one recording each.
    let mut paths: Vec<&std::path::Path> = vec![path];
    if let Some((experiment_path, _)) = &sources.experiment {
        paths.push(experiment_path);
    }
    let result = super::report_save::build_report(
        &paths,
        sources.anchor.as_ref(),
        keep.as_ref(),
        range,
        selection_json,
        events_json.as_deref(),
    );
    (result, "rezolus-report.dendro")
}

/// Convert a `spawn_blocking` outcome into a download Response, logging
/// success and the two failure modes (build error vs. task panic).
fn finalize_attachment(
    result: Result<Result<Vec<u8>, String>, tokio::task::JoinError>,
    filename: &'static str,
    attach: fn(&str, Vec<u8>) -> Response,
) -> Response {
    match result {
        Ok(built) => finalize_build(built, filename, attach),
        Err(e) => {
            error!("report task panicked: {e}");
            server_error("internal error")
        }
    }
}

/// Convert a build's result into a download Response, logging it.
fn finalize_build(
    result: Result<Vec<u8>, String>,
    filename: &'static str,
    attach: fn(&str, Vec<u8>) -> Response,
) -> Response {
    match result {
        Ok(output) => {
            info!("saved report {filename} ({} bytes)", output.len());
            attach(filename, output)
        }
        Err(e) => {
            error!("report build failed: {e}");
            server_error(format!("report build failed: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::dashboard::TemplateRegistry;

    fn upload_only_state() -> Arc<AppState> {
        let store: Arc<dyn MetricsSource> =
            Arc::new(metriken_query::MemoryStore::builder().build());
        Arc::new(AppState::new(store, TemplateRegistry::empty()))
    }

    /// An uploaded comparison fills its slots by arm, as file mode does.
    #[test]
    fn an_uploaded_comparison_fills_its_slots_by_arm() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rev.dendro");
        let labels: Vec<std::collections::BTreeMap<String, String>> =
            [("s0", "experiment"), ("s1", "baseline")]
                .iter()
                .map(|(s, arm)| {
                    [("source", *s), ("host", "h"), ("arm", *arm)]
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect()
                })
                .collect();
        crate::mcp::tests::multi_recording_with_labels(&path, &labels);
        let state = upload_only_state();
        let _ = ingest_rez_from_path(&state, path, "rev.dendro".to_string());
        let meta = |id: &str| state.captures.file_metadata_by_id(id).unwrap();
        assert!(meta("baseline").contains("s1"), "{}", meta("baseline"));
        assert!(meta("experiment").contains("s0"), "{}", meta("experiment"));
    }

    /// An uploaded experiment of several recordings is refused naming the
    /// uploaded baseline by the name it was uploaded as, quoted for a shell,
    /// not by its staged copy.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_multi_recording_experiment_upload_names_the_uploaded_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("staged-copy");
        ::rez::fixtures::multi_recording(&base, &[("redis", "web-01")]);
        let two = dir.path().join("two.dendro");
        ::rez::fixtures::multi_recording(&two, &[("redis", "web-02"), ("valkey", "web-03")]);
        let state = upload_only_state();
        let _ = ingest_rez_from_path(&state, base, "my base.dendro".to_string());

        let body = std::fs::read(&two).unwrap();
        let response = attach_upload(&state, &body, "exp.dendro".to_string());
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let text = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&text);
        assert!(
            text.contains("recording combine 'my base.dendro' exp.dendro -o both.dendro"),
            "{text}"
        );
        assert!(!text.contains("staged-copy"), "{text}");
    }

    /// An upload with one armed recording among unarmed ones is refused, and
    /// the view is left as it was.
    #[test]
    fn an_upload_with_one_arm_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.dendro");
        let labels: Vec<std::collections::BTreeMap<String, String>> = vec![
            [("source".to_string(), "a".to_string())].into(),
            [
                ("source".to_string(), "b".to_string()),
                ("arm".to_string(), "baseline".to_string()),
            ]
            .into(),
        ];
        crate::mcp::tests::multi_recording_with_labels(&path, &labels);
        let state = upload_only_state();
        let Json(reply) = ingest_rez_from_path(&state, path, "one.dendro".to_string());
        let reply = serde_json::to_value(&reply).unwrap();
        assert_eq!(reply["status"], "error", "{reply}");
        assert!(state.captures.get_by_id("experiment").is_none());
    }

    /// An upload of the concurrent A/B keeps the agent beside the
    /// comparison, as file mode does.
    #[test]
    fn an_uploaded_comparison_keeps_the_unarmed_recordings_beside() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::viewer::combined_test::concurrent_ab(dir.path());
        let state = upload_only_state();
        let _ = ingest_rez_from_path(&state, path, "conc.dendro".to_string());
        let meta = |id: &str| state.captures.file_metadata_by_id(id).unwrap();
        assert!(meta("baseline").contains("svcA"), "{}", meta("baseline"));
        assert!(
            meta("experiment").contains("svcB"),
            "{}",
            meta("experiment")
        );
        assert_eq!(state.captures.capture_ids(), ["baseline", "experiment"]);
        let routes: Vec<String> = state
            .sections
            .read()
            .sections()
            .iter()
            .map(|s| s.route.clone())
            .collect();
        assert!(routes.contains(&"/cpu@h".to_string()), "{routes:?}");
        let info = state.captures.systeminfo_by_id("baseline").unwrap();
        assert!(info.contains(r#""h":{"#), "{info}");
    }

    /// An uploaded report of the concurrent A/B, its selection on the
    /// archive's first recording (the unarmed agent, as reports were saved
    /// before the baseline's), opens with its pins and in report mode.
    #[test]
    fn an_uploaded_report_finds_its_selection_on_any_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::viewer::combined_test::concurrent_ab(dir.path());
        let keep: std::collections::BTreeSet<String> =
            ["cpu_usage".to_string()].into_iter().collect();
        let bytes = ::report_save::build_report(
            ::report_save::ReportSource::Dendro(&std::fs::read(&path).unwrap()),
            None,
            Some(&keep),
            None,
            r#"{"entries":[]}"#,
            Some(r#"{"events":[{"timestamp":1700000002000000000,"description":"old"}]}"#),
        )
        .unwrap();
        let report = dir.path().join("report.dendro");
        std::fs::write(&report, bytes).unwrap();
        let state = upload_only_state();
        let _ = ingest_rez_from_path(&state, report, "report.dendro".to_string());
        assert_eq!(state.selection.read().as_deref(), Some(r#"{"entries":[]}"#));
        assert_eq!(
            state.trimmed_report_marker.read().as_deref(),
            Some("trimmed")
        );
        // Its events reach the baseline's metadata, where the page reads them.
        let meta: serde_json::Value =
            serde_json::from_str(&state.captures.file_metadata_by_id("baseline").unwrap()).unwrap();
        assert_eq!(meta["events"]["events"][0]["description"], "old", "{meta}");
    }

    /// Save as Report through the handler; the report's bytes.
    async fn save(state: &Arc<AppState>, body: &str) -> Vec<u8> {
        let response = save_with_selection(State(Arc::clone(state)), body.to_string()).await;
        assert_eq!(response.status(), StatusCode::OK);
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    /// The `source` of each recording of a dendro report that carries a
    /// selection.
    fn marked(bytes: &[u8]) -> Vec<String> {
        dendro::archive::Archive::open_bytes(bytes.to_vec())
            .unwrap()
            .read_sources()
            .unwrap()
            .into_iter()
            .filter(|s| {
                s.meta
                    .metadata
                    .contains_key(crate::parquet_metadata::KEY_SELECTION)
            })
            .map(|s| s.meta.labels["source"].clone())
            .collect()
    }

    /// An uploaded comparison saves its report onto its baseline's
    /// recording, and replacing the baseline forgets which that was.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn an_uploaded_comparison_saves_its_report_on_the_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let path = crate::viewer::combined_test::concurrent_ab(dir.path());
        let state = upload_only_state();
        let _ = ingest_rez_from_path(&state, path.clone(), "conc.dendro".to_string());
        *state.parquet_path.write() = Some(path);
        let bytes = save(&state, r#"{"entries":[],"trim_columns":false}"#).await;
        assert_eq!(marked(&bytes), ["svcA"]);
        state.replace_baseline(state.baseline_data());
        assert!(state.anchor_labels.read().is_none());
    }

    /// A report saved from a comparison chosen by --baseline/--experiment of
    /// an archive without arms sits on svcA, not the first recording; the
    /// archive reopens combined, and the selection, marker and events are
    /// found there, in file mode, an upload and the WASM viewer. Saving
    /// again strips an earlier report's keys from every other recording.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_report_of_a_selected_comparison_reopens_combined() {
        use crate::parquet_metadata::{KEY_EVENTS, KEY_REPORT};
        use rez::fixtures::{one_run, Recording};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unarmed.dendro");
        let rec = |labels: &'static [(&'static str, &'static str)], cpus| Recording {
            labels,
            metadata: &[("source", "rezolus")],
            cpus,
            busy_ns: 250_000_000,
            counter: None,
        };
        one_run(
            &path,
            &[
                rec(&[("source", "rezolus"), ("host", "h")], 1),
                rec(&[("source", "svcA"), ("host", "h")], 1),
                rec(&[("source", "svcB"), ("host", "h")], 1),
            ],
        );
        let state = Arc::new(crate::viewer::combined_test::view(
            &path,
            &["--baseline", "source=svcA", "--experiment", "source=svcB"],
        ));
        let body = r#"{"entries":[],"trim_columns":true,"events":[{"timestamp":1700000002000000000,"description":"rollout"}]}"#;
        let bytes = save(&state, body).await;
        assert_eq!(marked(&bytes), ["svcA"]);
        let report = dir.path().join("report.dendro");
        std::fs::write(&report, &bytes).unwrap();

        let check = |state: &AppState, how: &str| {
            assert_eq!(state.selection.read().as_deref(), Some(body), "{how}");
            assert_eq!(
                state.trimmed_report_marker.read().as_deref(),
                Some("trimmed"),
                "{how}"
            );
            let meta: serde_json::Value =
                serde_json::from_str(&state.captures.file_metadata_by_id("baseline").unwrap())
                    .unwrap();
            assert_eq!(
                meta[KEY_EVENTS]["events"][0]["description"], "rollout",
                "{how}: {meta}"
            );
        };
        let reopened = crate::viewer::combined_test::view(&report, &[]);
        assert!(!reopened.members.read().is_empty(), "opens combined");
        check(&reopened, "file mode");
        let uploaded = upload_only_state();
        let _ = ingest_rez_from_path(&uploaded, report.clone(), "report.dendro".to_string());
        check(&uploaded, "upload");
        assert!(uploaded.combined_catalog.read().is_some());
        uploaded.replace_baseline(uploaded.baseline_data());
        assert!(uploaded.combined_catalog.read().is_none());

        let mut reg = viewer::WasmCaptureRegistry::new();
        reg.attach("baseline", &bytes, "report.dendro").unwrap();
        assert_eq!(reg.selection("baseline").as_deref(), Some(body));
        let meta: serde_json::Value =
            serde_json::from_str(&reg.file_metadata_json("baseline").unwrap()).unwrap();
        assert_eq!(meta[KEY_REPORT], "trimmed");
        assert_eq!(meta[KEY_EVENTS]["events"][0]["description"], "rollout");

        // An earlier report saved onto the first recording, then a new one
        // from the comparison: only svcA carries a report.
        let keep: std::collections::BTreeSet<String> =
            ["cpu_usage".to_string()].into_iter().collect();
        let old = ::report_save::build_report(
            ::report_save::ReportSource::Dendro(&std::fs::read(&path).unwrap()),
            None,
            Some(&keep),
            None,
            r#"{"entries":[]}"#,
            Some(r#"{"events":[]}"#),
        )
        .unwrap();
        assert_eq!(marked(&old), ["rezolus"]);
        let old_path = dir.path().join("old.dendro");
        std::fs::write(&old_path, &old).unwrap();
        let state = Arc::new(crate::viewer::combined_test::view(
            &old_path,
            &["--baseline", "source=svcA", "--experiment", "source=svcB"],
        ));
        let again = save(&state, body).await;
        assert_eq!(marked(&again), ["svcA"]);
        let archive = dendro::archive::Archive::open_bytes(again).unwrap();
        for r in archive.read_sources().unwrap() {
            let has = |k: &str| r.meta.metadata.contains_key(k);
            let anchor = r.meta.labels["source"] == "svcA";
            assert_eq!(has(KEY_EVENTS), anchor, "{:?}", r.meta.labels);
            assert_eq!(has(KEY_REPORT), anchor, "{:?}", r.meta.labels);
        }
    }

    /// An uploaded beside recording no label names is named by its archive
    /// position, as in file mode.
    #[test]
    fn an_uploaded_beside_recording_without_labels_is_named_by_position() {
        use rez::fixtures::{one_run, Recording};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pos.dendro");
        let rec = |labels: &'static [(&'static str, &'static str)]| Recording {
            labels,
            metadata: &[("source", "rezolus")],
            cpus: 1,
            busy_ns: 250_000_000,
            counter: None,
        };
        one_run(
            &path,
            &[
                rec(&[("host", "a"), ("arm", "baseline")]),
                rec(&[("host", "b"), ("arm", "experiment")]),
                rec(&[("zone", "z")]),
            ],
        );
        for state in [
            {
                let state = upload_only_state();
                let _ = ingest_rez_from_path(&state, path.clone(), "pos.dendro".to_string());
                state
            },
            Arc::new(crate::viewer::combined_test::view(&path, &[])),
        ] {
            let routes: Vec<String> = state
                .sections
                .read()
                .sections()
                .iter()
                .map(|s| s.route.clone())
                .collect();
            assert!(
                routes.contains(&"/cpu@recording-3".to_string()),
                "{routes:?}"
            );
        }
    }

    /// Save as Report returns a `.dendro` report carrying the selection,
    /// through the handler.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn save_with_selection_returns_a_dendro_report() {
        let dir = tempfile::tempdir().unwrap();
        let src = crate::parquet_tools::test_support::simple_dendro(dir.path());
        let state = upload_only_state();
        *state.parquet_path.write() = Some(src);

        let body = r#"{"entries":[],"trim_columns":false}"#;
        let response = save_with_selection(State(Arc::clone(&state)), body.to_string()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let disposition = response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(
            disposition.contains("rezolus-report.dendro"),
            "{disposition}"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let archive = dendro::archive::Archive::open_bytes(bytes.to_vec()).unwrap();
        let md = &archive.read_sources().unwrap()[0].meta.metadata;
        assert_eq!(
            md.get(crate::parquet_metadata::KEY_SELECTION)
                .map(String::as_str),
            Some(body)
        );
    }

    /// An upload is opened by its content: a dendro archive loads, a `.rez`
    /// is refused with the upgrade command, whatever either is called.
    #[tokio::test]
    async fn an_upload_is_a_dendro_archive_and_a_rez_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let rez = std::fs::read(::rez::fixtures::committed_rez("v3.rez", dir.path())).unwrap();
        let dendro = std::fs::read(crate::parquet_tools::test_support::simple_dendro(
            dir.path(),
        ))
        .unwrap();
        let named = |name: &str| {
            let mut h = HeaderMap::new();
            h.insert("x-rezolus-filename", name.parse().unwrap());
            h
        };

        let state = upload_only_state();
        let response = upload_parquet(
            State(Arc::clone(&state)),
            named("old.rez"),
            Bytes::from(rez),
        )
        .await;
        let value = serde_json::to_value(&response.0).unwrap();
        assert_eq!(value["status"], "error", "{value}");
        assert!(
            value["error"]
                .as_str()
                .is_some_and(|e| e.contains("rezolus recording upgrade old.rez")),
            "{value}"
        );
        let response = upload_parquet(State(state), named("x.parquet"), Bytes::from(dendro)).await;
        let value = serde_json::to_value(&response.0).unwrap();
        assert_eq!(value["status"], "success", "{value}");
    }
}
