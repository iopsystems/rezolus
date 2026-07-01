use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Child;

/// Spawn a wrapped command with inherited stdio (stdin/stdout/stderr pass
/// straight through to the terminal, like `perf record` / `time`).
pub fn spawn(command: &[String]) -> std::io::Result<Child> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;
    tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
}

/// Terminate a running child: SIGTERM, wait up to `grace`, then SIGKILL.
/// Returns the child's final `ExitStatus`.
pub async fn terminate(child: &mut Child, grace: Duration) -> ExitStatus {
    if let Some(pid) = child.id() {
        // SAFETY: sending a signal to a pid we own; return value ignored
        // because the child may have already exited between checks.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(Ok(status)) => status,
        _ => {
            // Grace elapsed or wait errored: force kill and reap.
            let _ = child.kill().await;
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
}
