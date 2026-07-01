use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::time::Duration;

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
}
