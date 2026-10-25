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
    slot(&slots, "baseline");
    slot(&slots, "experiment");
}
