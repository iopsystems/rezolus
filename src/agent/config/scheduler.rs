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
            // Mismatched policy and parameters
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
        match self.parameters {
            Parameters::Realtime { priority } => self.set_realtime_scheduler(priority),
            Parameters::Normal { niceness } => self.set_normal_scheduler(niceness),
        }
    }

    #[cfg(target_os = "linux")]
    fn set_realtime_scheduler(&self, priority: i32) {
        let policy = match self.policy {
            Policy::Fifo => SCHED_FIFO,
            Policy::RoundRobin => SCHED_RR,
            _ => unreachable!(),
        } | SCHED_RESET_ON_FORK;

        let param = libc::sched_param {
            sched_priority: priority,
        };

        let result = unsafe { libc::sched_setscheduler(0 as libc::pid_t, policy, &param) };

        if result == -1 {
            let errno = std::io::Error::last_os_error();
            match errno.raw_os_error().unwrap_or(0) {
                libc::EPERM => {
                    eprintln!("could not set scheduler policy to realtime: permission denied");
                    std::process::exit(1);
                }
                _ => {
                    eprintln!(
                        "could not set scheduler policy to realtime: {}",
                        errno.to_string()
                    );
                    std::process::exit(1);
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn set_normal_scheduler(&self, niceness: i32) {
        let policy = SCHED_NORMAL | SCHED_RESET_ON_FORK;

        // normal policy uses priority of 0
        let param = libc::sched_param { sched_priority: 0 };

        let result = unsafe { libc::sched_setscheduler(0 as libc::pid_t, policy, &param) };

        if result == -1 {
            let errno = std::io::Error::last_os_error();
            eprintln!(
                "could not set scheduler policy to normal: {}",
                errno.to_string()
            );
            std::process::exit(1);
        }

        // set the niceness
        let result = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0 as libc::id_t, niceness) };
        if result == -1 {
            let errno = std::io::Error::last_os_error();
            eprintln!("could not set niceness: {}", errno.to_string());
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
