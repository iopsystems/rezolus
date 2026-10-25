//! A two-file comparison is one recording per file. A file holding several
//! is refused with the commands that compare two of its recordings; each
//! test runs those commands as printed, through a shell, and checks which
//! recording the viewer puts in each slot.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_rezolus");

/// `line`, a command as the refusal prints it (`rezolus ...`), run by a
/// shell in `dir` with the program replaced by the built binary.
fn shell(dir: &Path, line: &str) -> Command {
    let rest = line.strip_prefix("rezolus ").expect(line);
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!("exec '{BIN}' {rest}"))
        .current_dir(dir)
        .env("REZOLUS_NO_OPEN", "1");
    cmd
}

fn run(dir: &Path, line: &str) -> Output {
    let out = shell(dir, line).output().unwrap();
    assert!(
        out.status.success(),
        "{line}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// The refusal of `view first second`, and the commands it prints, one per
/// indented line.
fn refusal(dir: &Path, first: &str, second: &str) -> (String, Vec<String>) {
    let out = Command::new(BIN)
        .args(["view", first, second])
        .current_dir(dir)
        .env("REZOLUS_NO_OPEN", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let commands = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("  "))
        .filter(|l| l.starts_with("rezolus "))
        .map(str::to_string)
        .collect();
    (stderr, commands)
}

struct Viewer {
    child: Child,
    addr: String,
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `line` (a `rezolus view ...` command) served on a free port, once it
/// says where to browse.
fn serve(dir: &Path, line: &str) -> Viewer {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let mut child = shell(dir, &format!("{line} --listen {addr}"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let viewer = Viewer { child, addr };
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(l) if l.contains("Use your browser to view") => return viewer,
            Ok(l) => seen.push(l),
            Err(_) => panic!("{line} did not serve:\n{}", seen.join("\n")),
        }
    }
}

/// The alias of each capture slot the viewer shows, by slot id.
fn slots(viewer: &Viewer) -> Vec<(String, String)> {
    let mut stream = TcpStream::connect(&viewer.addr).unwrap();
    write!(
        stream,
        "GET /api/v1/captures HTTP/1.0\r\nHost: {}\r\n\r\n",
        viewer.addr
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(head.contains(" 200 "), "{head}");
    let json: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"));
    let list = json.get("data").unwrap_or(&json);
    list.as_array()
        .unwrap_or_else(|| panic!("{json}"))
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_string(),
                c["alias"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// POST `body` to `path` as an upload named `name`: the status line and
/// the response body.
fn upload(viewer: &Viewer, path: &str, name: &str, body: &[u8]) -> (String, String) {
    let mut stream = TcpStream::connect(&viewer.addr).unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.0\r\nHost: {}\r\nx-rezolus-filename: {name}\r\n\
         Content-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
        viewer.addr,
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    // A report download is binary; only its status line is read.
    let response = String::from_utf8_lossy(&response);
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

fn slot<'a>(slots: &'a [(String, String)], id: &str) -> &'a str {
    &slots
        .iter()
        .find(|(i, _)| i == id)
        .unwrap_or_else(|| panic!("{id} in {slots:?}"))
        .1
}

/// Follow the label path the refusal of `view first second` prints:
/// combine, describe, then view with `baseline` and `experiment` (hosts)
/// as the selectors; the slots hold those hosts.
fn follow_by_labels(dir: &Path, first: &str, second: &str, baseline: &str, experiment: &str) {
    let (stderr, commands) = refusal(dir, first, second);
    assert_eq!(commands.len(), 5, "{stderr}");
    run(dir, &commands[0]);
    let listing = String::from_utf8_lossy(&run(dir, &commands[1]).stdout).to_string();
    for host in [baseline, experiment] {
        let selector = format!("--recording host={host}");
        assert!(listing.contains(&selector), "{selector} in:\n{listing}");
    }
    let view = commands[2]
        .replacen(
            "--baseline KEY=VALUE",
            &format!("--baseline host={baseline}"),
            1,
        )
        .replacen(
            "--experiment KEY=VALUE",
            &format!("--experiment host={experiment}"),
            1,
        );
    let viewer = serve(dir, &view);
    let slots = slots(&viewer);
    assert_eq!(slot(&slots, "baseline"), baseline, "{slots:?}");
    assert_eq!(slot(&slots, "experiment"), experiment, "{slots:?}");
}

#[test]
fn a_multi_recording_experiment_is_refused_with_commands_that_work() {
    let dir = tempfile::tempdir().unwrap();
    rez::fixtures::multi_recording(&dir.path().join("one.dendro"), &[("redis", "web-01")]);
    rez::fixtures::multi_recording(
        &dir.path().join("two.dendro"),
        &[("redis", "web-02"), ("valkey", "web-03")],
    );
    follow_by_labels(dir.path(), "one.dendro", "two.dendro", "web-01", "web-03");
}

/// The multi-recording file is the baseline, and its name needs quoting.
#[test]
fn a_multi_recording_baseline_is_refused_with_commands_that_work() {
    let dir = tempfile::tempdir().unwrap();
    rez::fixtures::multi_recording(
        &dir.path().join("two files.dendro"),
        &[("redis", "web-02"), ("valkey", "web-03")],
    );
    rez::fixtures::multi_recording(&dir.path().join("one.dendro"), &[("redis", "web-01")]);
    follow_by_labels(
        dir.path(),
        "two files.dendro",
        "one.dendro",
        "web-02",
        "web-01",
    );
}

/// The baseline and one of the experiment's recordings carry the same
/// labels, so after combining neither can be selected by labels: the
/// position path labels the baseline's (position 0) and the experiment's
/// other recording (position 2) as the arms.
#[test]
fn recordings_labels_cannot_tell_apart_compare_by_position() {
    let dir = tempfile::tempdir().unwrap();
    rez::fixtures::multi_recording(&dir.path().join("one.dendro"), &[("rezolus", "web-01")]);
    rez::fixtures::multi_recording(
        &dir.path().join("two.dendro"),
        &[("rezolus", "web-01"), ("rezolus", "web-02")],
    );
    let (stderr, commands) = refusal(dir.path(), "one.dendro", "two.dendro");
    assert_eq!(commands.len(), 5, "{stderr}");
    run(dir.path(), &commands[0]);
    let listing = String::from_utf8_lossy(&run(dir.path(), &commands[1]).stdout).to_string();
    assert!(
        listing.contains("cannot be selected by labels"),
        "{listing}"
    );
    assert!(listing.contains("(position 0)"), "{listing}");
    run(
        dir.path(),
        &commands[3]
            .replacen("baseline=N", "baseline=0", 1)
            .replacen("experiment=M", "experiment=2", 1),
    );
    let described = run(dir.path(), "rezolus mcp describe-recording ab.dendro");
    let described = String::from_utf8_lossy(&described.stdout).to_string();
    for (arm, host) in [("baseline", "web-01"), ("experiment", "web-02")] {
        let line = described
            .lines()
            .find(|l| l.contains(&format!("arm={arm}")))
            .unwrap_or_else(|| panic!("arm={arm} in:\n{described}"));
        assert!(line.contains(&format!("host={host}")), "{line}");
    }
    let viewer = serve(dir.path(), &commands[4]);
    let slots = slots(&viewer);
    // The aliases are the arm values; describe-recording above tied each arm
    // to its host.
    assert_eq!(slot(&slots, "baseline"), "baseline", "{slots:?}");
    assert_eq!(slot(&slots, "experiment"), "experiment", "{slots:?}");
}

/// A recording with no rows is left out of describe-recording's list but
/// keeps its archive position, which `--ab` counts: the listing prints each
/// recording's position, so the one meant is the one labelled.
#[test]
fn positions_count_a_recording_with_no_rows() {
    let dir = tempfile::tempdir().unwrap();
    rez::fixtures::multi_recording(&dir.path().join("one.dendro"), &[("rezolus", "web-01")]);
    let mut sources = rez::fixtures::multi_recording_sources(&[
        ("rezolus", "web-00"),
        ("rezolus", "web-01"),
        ("rezolus", "web-02"),
    ]);
    sources[0].rows.clear();
    rez::fixtures::write_dendro(
        &dir.path().join("two.dendro"),
        &sources,
        None,
        rez::fixtures::Finish::Finalized,
    );
    let (stderr, commands) = refusal(dir.path(), "one.dendro", "two.dendro");
    assert_eq!(commands.len(), 5, "{stderr}");
    run(dir.path(), &commands[0]);
    let listing = String::from_utf8_lossy(&run(dir.path(), &commands[1]).stdout).to_string();
    // web-02's position, as describe-recording prints it.
    let line = listing
        .lines()
        .find(|l| l.contains("host=web-02"))
        .unwrap_or_else(|| panic!("web-02 in:\n{listing}"));
    let position = line
        .split("(position ")
        .nth(1)
        .and_then(|r| r.split(')').next())
        .unwrap_or_else(|| panic!("a position in {line:?}"));
    assert_eq!(
        position, "3",
        "the rowless web-00 is position 1:\n{listing}"
    );
    run(
        dir.path(),
        &commands[3]
            .replacen("baseline=N", "baseline=0", 1)
            .replacen("experiment=M", &format!("experiment={position}"), 1),
    );
    let described = run(dir.path(), "rezolus mcp describe-recording ab.dendro");
    let described = String::from_utf8_lossy(&described.stdout).to_string();
    let line = described
        .lines()
        .find(|l| l.contains("arm=experiment"))
        .unwrap_or_else(|| panic!("arm=experiment in:\n{described}"));
    assert!(line.contains("host=web-02"), "{line}");
}

/// `method` on `path` with an empty body: the status line.
fn request(viewer: &Viewer, method: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(&viewer.addr).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.0\r\nHost: {}\r\nContent-Length: 0\r\n\r\n",
        viewer.addr
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response.lines().next().unwrap_or_default().to_string()
}

/// An uploaded experiment is one recording. Beside a one-recording baseline
/// or a comparison it fills the experiment slot; beside a combined view of
/// several recordings it is refused and the view is unchanged, whether the
/// baseline came from the command line or an upload.
#[test]
fn an_uploaded_experiment_is_refused_beside_a_combined_view() {
    let dir = tempfile::tempdir().unwrap();
    rez::fixtures::multi_recording(&dir.path().join("one.dendro"), &[("redis", "web-01")]);
    rez::fixtures::multi_recording(&dir.path().join("exp.dendro"), &[("redis", "web-09")]);
    rez::fixtures::multi_recording(&dir.path().join("b.dendro"), &[("valkey", "web-02")]);
    rez::fixtures::multi_recording(
        &dir.path().join("fleet.dendro"),
        &[("redis", "web-01"), ("valkey", "web-01")],
    );
    run(
        dir.path(),
        "rezolus recording combine one.dendro b.dendro \
         --ab baseline=one.dendro experiment=b.dendro -o ab.dendro",
    );
    let read = |name: &str| std::fs::read(dir.path().join(name)).unwrap();
    let exp = read("exp.dendro");
    let refused = |viewer: &Viewer| {
        let before = slots(viewer);
        let (status, body) = upload(viewer, "/api/v1/captures/experiment", "exp.dendro", &exp);
        assert!(status.contains(" 400 "), "{status}: {body}");
        assert!(
            body.starts_with("fleet.dendro holds 2 recordings; the baseline"),
            "{body}"
        );
        assert!(
            body.contains("rezolus recording combine fleet.dendro exp.dendro -o both.dendro"),
            "{body}"
        );
        assert_eq!(slots(viewer), before);
    };
    let attached = |viewer: &Viewer| {
        let (status, body) = upload(viewer, "/api/v1/captures/experiment", "exp.dendro", &exp);
        assert!(status.contains(" 200 "), "{status}: {body}");
        assert_eq!(slots(viewer).len(), 2, "{:?}", slots(viewer));
    };

    attached(&serve(dir.path(), "rezolus view one.dendro"));
    refused(&serve(dir.path(), "rezolus view fleet.dendro"));

    // Uploaded baselines: combined, then one recording in its place.
    let viewer = serve(dir.path(), "rezolus view");
    let (status, body) = upload(
        &viewer,
        "/api/v1/upload",
        "fleet.dendro",
        &read("fleet.dendro"),
    );
    assert!(status.contains(" 200 "), "{status}: {body}");
    refused(&viewer);
    let (status, body) = upload(&viewer, "/api/v1/upload", "one.dendro", &read("one.dendro"));
    assert!(status.contains(" 200 "), "{status}: {body}");
    attached(&viewer);

    // A comparison's experiment arm is replaced, as the page's Load button
    // does it: DELETE, then POST.
    let viewer = serve(dir.path(), "rezolus view ab.dendro");
    assert_eq!(slots(&viewer).len(), 2);
    let filename = |capture: &str| {
        let mut stream = TcpStream::connect(&viewer.addr).unwrap();
        write!(
            stream,
            "GET /api/v1/metadata?capture={capture} HTTP/1.0\r\nHost: {}\r\n\r\n",
            viewer.addr
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let body = response.split_once("\r\n\r\n").unwrap().1;
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        json["data"]["filename"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let baseline = filename("baseline");
    assert_ne!(filename("experiment"), "exp.dendro");
    let status = request(&viewer, "DELETE", "/api/v1/captures/experiment");
    assert!(status.contains(" 200 "), "{status}");
    attached(&viewer);
    assert_eq!(filename("experiment"), "exp.dendro");
    assert_eq!(filename("baseline"), baseline);

    // A report copies whole files, so after the replacement none is saved.
    let (status, body) = upload(
        &viewer,
        "/api/v1/save_with_selection",
        "selection.json",
        br#"{"entries":[]}"#,
    );
    assert!(
        body.contains("Save as Report cannot save this comparison: ab.dendro holds 2 recordings"),
        "{status}: {body}"
    );

    // A two-recording experiment beside the A/B archive is refused with the
    // commands, and the note that `--ab` keeps an arm a recording carries.
    rez::fixtures::multi_recording(
        &dir.path().join("exp2.dendro"),
        &[("redis", "web-01"), ("rezolus", "web-09")],
    );
    let status = request(&viewer, "DELETE", "/api/v1/captures/experiment");
    assert!(status.contains(" 200 "), "{status}");
    let (status, body) = upload(
        &viewer,
        "/api/v1/captures/experiment",
        "exp2.dendro",
        &read("exp2.dendro"),
    );
    assert!(status.contains(" 400 "), "{status}: {body}");
    assert!(body.starts_with("exp2.dendro holds 2 recordings"), "{body}");
    assert!(body.contains("--ab baseline=N experiment=M"), "{body}");
    assert!(body.contains("keeps an `arm` label"), "{body}");

    // With no experiment attached, the archive saves again.
    let (status, body) = upload(
        &viewer,
        "/api/v1/save_with_selection",
        "selection.json",
        br#"{"entries":[]}"#,
    );
    assert!(body.starts_with("SQLite format 3"), "{status}: {body:.200}");
}

/// Before any replacement, a comparison archive saves a report.
#[test]
fn a_comparison_archive_saves_a_report() {
    let dir = tempfile::tempdir().unwrap();
    rez::fixtures::multi_recording(&dir.path().join("a.dendro"), &[("redis", "web-01")]);
    rez::fixtures::multi_recording(&dir.path().join("b.dendro"), &[("valkey", "web-02")]);
    run(
        dir.path(),
        "rezolus recording combine a.dendro b.dendro \
         --ab baseline=a.dendro experiment=b.dendro -o ab.dendro",
    );
    let viewer = serve(dir.path(), "rezolus view ab.dendro");
    let (status, body) = upload(
        &viewer,
        "/api/v1/save_with_selection",
        "selection.json",
        br#"{"entries":[]}"#,
    );
    assert!(status.contains(" 200 "), "{status}");
    // A dendro archive is an SQLite database.
    assert!(
        body.starts_with("SQLite format 3"),
        "{}",
        &body[..body.len().min(200)]
    );
}

/// The baseline is an A/B archive named `ab.dendro`, as the position path
/// writes one: the refusal's output names avoid it, it notes that `--ab`
/// keeps an arm, and the label path it prints works.
#[test]
fn an_ab_archive_baseline_is_refused_with_commands_that_work() {
    let dir = tempfile::tempdir().unwrap();
    rez::fixtures::multi_recording(&dir.path().join("a.dendro"), &[("redis", "web-01")]);
    rez::fixtures::multi_recording(&dir.path().join("b.dendro"), &[("valkey", "web-02")]);
    rez::fixtures::multi_recording(&dir.path().join("exp.dendro"), &[("redis", "web-09")]);
    run(
        dir.path(),
        "rezolus recording combine a.dendro b.dendro \
         --ab baseline=a.dendro experiment=b.dendro -o ab.dendro",
    );
    let (stderr, commands) = refusal(dir.path(), "ab.dendro", "exp.dendro");
    assert_eq!(commands.len(), 5, "{stderr}");
    assert!(commands[3].ends_with("-o ab-2.dendro"), "{stderr}");
    assert!(stderr.contains("keeps an `arm` label"), "{stderr}");
    run(dir.path(), &commands[0]);
    let listing = String::from_utf8_lossy(&run(dir.path(), &commands[1]).stdout).to_string();
    assert!(listing.contains("--recording host=web-09"), "{listing}");
    let view = commands[2]
        .replacen("--baseline KEY=VALUE", "--baseline arm=baseline", 1)
        .replacen("--experiment KEY=VALUE", "--experiment host=web-09", 1);
    let viewer = serve(dir.path(), &view);
    let slots = slots(&viewer);
    assert_eq!(slots.len(), 2, "{slots:?}");
    assert_eq!(slot(&slots, "experiment"), "web-09", "{slots:?}");
}

/// The baseline's one recording carries `arm=baseline` and the experiment's
/// two recordings cannot be told apart by labels: the printed position step
/// labels the arms, and the archive it writes opens as the comparison.
#[test]
fn the_position_step_works_beside_an_arm_label() {
    let dir = tempfile::tempdir().unwrap();
    let mut armed = rez::fixtures::multi_recording_sources(&[("redis", "web-01")]);
    armed[0]
        .labels
        .insert("arm".to_string(), "baseline".to_string());
    rez::fixtures::write_dendro(
        &dir.path().join("armed.dendro"),
        &armed,
        None,
        rez::fixtures::Finish::Finalized,
    );
    rez::fixtures::multi_recording(
        &dir.path().join("dup.dendro"),
        &[("rezolus", "web-02"), ("rezolus", "web-02")],
    );
    let (stderr, commands) = refusal(dir.path(), "armed.dendro", "dup.dendro");
    assert_eq!(commands.len(), 5, "{stderr}");
    run(dir.path(), &commands[0]);
    let listing = String::from_utf8_lossy(&run(dir.path(), &commands[1]).stdout).to_string();
    assert!(
        listing.contains("cannot be selected by labels"),
        "{listing}"
    );
    run(
        dir.path(),
        &commands[3]
            .replacen("baseline=N", "baseline=0", 1)
            .replacen("experiment=M", "experiment=1", 1),
    );
    let viewer = serve(dir.path(), &commands[4]);
    let slots = slots(&viewer);
    assert_eq!(slot(&slots, "baseline"), "baseline", "{slots:?}");
    assert_eq!(slot(&slots, "experiment"), "experiment", "{slots:?}");
}
