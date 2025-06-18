use super::*;

fn priority() -> i32 {
    15
}

fn niceness() -> i32 {
    0
}

#[cfg(target_os = "linux")]
use libc::{SCHED_FIFO, SCHED_NORMAL, SCHED_RESET_ON_FORK, SCHED_RR};

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Scheduler {
    pub policy: Policy,
    #[serde(flatten)]
    pub parameters: Parameters,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    Normal,
    Fifo,
    RoundRobin,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
pub enum Parameters {
    Realtime {
        #[serde(default = "priority")]
        priority: i32,
    },
    Normal {
        #[serde(default = "niceness")]
        niceness: i32,
    },
}

impl Scheduler {
    pub fn check(&self) {
        match (&self.policy, &self.parameters) {
            (Policy::Fifo | Policy::RoundRobin, Parameters::Realtime { priority }) => {
                if !(1..=99).contains(priority) {
                    eprintln!("priority must be in the range 1..=99, got {}", priority);
                    std::process::exit(1);
                }
            }
            (Policy::Normal, Parameters::Normal { niceness }) => {
                if !(-20..=19).contains(niceness) {
                    eprintln!("niceness must be in the range -20..=19, got {}", niceness);
                    std::process::exit(1);
                }
            }
            _ => {
                eprintln!(
                    "scheduler policy {:?} is incompatible with the provided parameters",
                    self.policy
                );
            }
        }
    }

    /// Apply this scheduling configuration
    #[cfg(target_os = "linux")]
    pub fn apply(&self) {
        self.set_scheduler();

        if let Parameters::Normal { niceness } = self.parameters {
            let result = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, niceness) };

            if result == -1 {
                let e = std::io::Error::last_os_error();
                eprintln!("could not set niceness: {e}");
                std::process::exit(1);
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn set_scheduler(&self) {
        let policy = match self.policy {
            Policy::Fifo => SCHED_FIFO,
            Policy::RoundRobin => SCHED_RR,
            Policy::Normal => SCHED_NORMAL,
        } | SCHED_RESET_ON_FORK;

        let priority = match self.parameters {
            Parameters::Realtime { priority } => priority,
            Parameters::Normal { .. } => 0,
        };

        let param = libc::sched_param {
            sched_priority: priority,
        };

        let result = unsafe { libc::sched_setscheduler(0, policy, &param) };

        if result == -1 {
            let e = std::io::Error::last_os_error();
            eprintln!(
                "could not set scheduler policy: {:?} error: {e}",
                self.policy
            );
            std::process::exit(1);
        }
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler {
            policy: Policy::RoundRobin,
            parameters: Parameters::Realtime {
                priority: priority(),
            },
        }
    }
}
