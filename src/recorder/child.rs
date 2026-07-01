use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Child;

/// Spawn a wrapped command with inherited stdio (stdin/stdout/stderr pass
/// straight through to the terminal, like `perf record` / `time`).
///
/// The child is made the leader of a new process group (`process_group(0)`, so
/// its pgid equals its pid) so that [`terminate`] can signal the entire group —
/// reaching workers a shell or benchmark harness forks, not just the direct
/// child.
pub fn spawn(command: &[String]) -> std::io::Result<Child> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;
    tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn()
}

/// Terminate a running child's process group: SIGTERM, wait up to `grace`, then
/// SIGKILL. Signals the whole group (see [`spawn`]) so forked workers are
/// cleaned up too. Returns the child leader's final `ExitStatus`.
pub async fn terminate(child: &mut Child, grace: Duration) -> ExitStatus {
    // The child is its own process group leader, so its pid is the pgid.
    let pgid = child.id().map(|p| p as libc::pid_t);
    if let Some(pgid) = pgid {
        // SAFETY: signalling a process group we created; return value ignored
        // because members may have already exited between checks.
        unsafe {
            libc::killpg(pgid, libc::SIGTERM);
        }
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(status)) => status,
        _ => {
            // Grace elapsed or wait errored: force-kill the whole group and reap
            // the leader.
            if let Some(pgid) = pgid {
                unsafe {
                    libc::killpg(pgid, libc::SIGKILL);
                }
            }
            child
                .wait()
                .await
                .unwrap_or_else(|_| ExitStatus::from_raw(libc::SIGKILL))
        }
    }
}

/// Grace period between SIGTERM and SIGKILL when capping a wrapped command.
pub const TERM_GRACE: Duration = Duration::from_secs(2);
/// Exit code emitted when a wrapped command is killed by the --duration cap.
pub const CAP_EXIT_CODE: i32 = 124;

/// Why a wrapped-command recording ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The command exited on its own; carries the mapped process exit code.
    Exited(i32),
    /// The command outlived `--duration` and was killed.
    Capped,
}

impl Outcome {
    /// The process exit code Rezolus should return for this outcome.
    pub fn exit_code(self) -> i32 {
        match self {
            Outcome::Exited(code) => code,
            Outcome::Capped => CAP_EXIT_CODE,
        }
    }
}

/// Map a child `ExitStatus` to a shell-style process exit code.
/// Normal exit yields its code; signal death yields `128 + signal`.
pub fn map_exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        128 + sig
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn outcome_exit_codes() {
        assert_eq!(Outcome::Exited(0).exit_code(), 0);
        assert_eq!(Outcome::Exited(3).exit_code(), 3);
        assert_eq!(Outcome::Capped.exit_code(), 124);
    }

    #[test]
    fn map_exit_code_normal() {
        assert_eq!(map_exit_code(ExitStatus::from_raw(0)), 0);
        // raw status 0x0100 == exit code 1 on Unix wait() encoding
        assert_eq!(map_exit_code(ExitStatus::from_raw(1 << 8)), 1);
    }

    #[test]
    fn map_exit_code_signal() {
        // raw status 9 == killed by signal 9 (SIGKILL), no exit code
        assert_eq!(map_exit_code(ExitStatus::from_raw(9)), 128 + 9);
    }

    #[tokio::test]
    async fn spawn_true_exits_zero() {
        let mut child = spawn(&["true".to_string()]).unwrap();
        // Poll like the record loop does.
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(map_exit_code(status), 0);
    }

    #[tokio::test]
    async fn spawn_false_exits_nonzero() {
        let mut child = spawn(&["false".to_string()]).unwrap();
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(map_exit_code(status), 1);
    }

    #[tokio::test]
    async fn terminate_kills_long_sleep() {
        let mut child = spawn(&["sleep".to_string(), "30".to_string()]).unwrap();
        // Short grace so the test is fast; sleep ignores SIGTERM? No — default
        // disposition of SIGTERM terminates sleep, so grace path suffices.
        let start = std::time::Instant::now();
        let status = terminate(&mut child, Duration::from_millis(500)).await;
        assert!(start.elapsed() < Duration::from_secs(5), "terminate hung");
        // Killed by a signal, so no normal exit code.
        assert!(status.code().is_none());
    }

    #[tokio::test]
    async fn spawn_empty_command_errors() {
        assert!(spawn(&[]).is_err());
    }

    #[tokio::test]
    async fn terminate_kills_forked_grandchild() {
        use std::io::Read;

        // A shell backgrounds a grandchild `sleep` and records its pid, then
        // waits. `terminate` must kill the whole process group (shell + the
        // backgrounded sleep), not just the shell.
        let pid_file =
            std::env::temp_dir().join(format!("rz_grandchild_{}.pid", std::process::id()));
        let _ = std::fs::remove_file(&pid_file);
        let script = format!("sleep 300 & echo $! > {} ; wait", pid_file.display());
        let mut child = spawn(&["sh".to_string(), "-c".to_string(), script]).unwrap();

        // Wait for the shell to publish the grandchild pid.
        let gpid: i32 = loop {
            if let Ok(mut f) = std::fs::File::open(&pid_file) {
                let mut s = String::new();
                let _ = f.read_to_string(&mut s);
                if let Ok(pid) = s.trim().parse::<i32>() {
                    break pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        terminate(&mut child, Duration::from_millis(500)).await;
        // Give the kernel a moment to deliver signals and reap.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // kill(pid, 0) probes existence: it fails with ESRCH once the
        // grandchild is gone.
        let alive = unsafe { libc::kill(gpid, 0) } == 0;
        let _ = std::fs::remove_file(&pid_file);
        if alive {
            // Cleanup so a failing test doesn't leak the process.
            unsafe {
                libc::kill(gpid, libc::SIGKILL);
            }
        }
        assert!(!alive, "grandchild {gpid} survived group terminate");
    }
}
