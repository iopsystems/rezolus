use super::*;

fn priority() -> i32 {
    15
}

fn niceness() -> i32 {
    0
}

#[cfg(target_os = "linux")]
use libc::{SCHED_FIFO, SCHED_NORMAL, SCHED_RR};

#[derive(Debug, Clone)]
pub enum Scheduler {
    Normal { niceness: i32 },
    Fifo { priority: i32 },
    RoundRobin { priority: i32 },
}

impl Scheduler {
    pub fn check(&self) {
        match self {
            Self::Normal { niceness } => {
                if !(-20..=19).contains(niceness) {
                    eprintln!("niceness must be in the range -20..=19, got {}", niceness);
                    std::process::exit(1);
                }
            }
            Self::Fifo { priority } | Self::RoundRobin { priority } => {
                if !(1..=99).contains(priority) {
                    eprintln!("priority must be in the range 1..=99, got {}", priority);
                    std::process::exit(1);
                }
            }
        }
    }

    /// Apply this scheduling configuration
    #[cfg(target_os = "linux")]
    pub fn apply(&self) {
        self.set_scheduler();

        if let Self::Normal { niceness } = self {
            let result = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, *niceness) };

            if result == -1 {
                let e = std::io::Error::last_os_error();
                eprintln!("could not set niceness: {e}");
                std::process::exit(1);
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn set_scheduler(&self) {
        let (policy, priority) = match self {
            Self::Normal { .. } => (SCHED_NORMAL, 0),
            Self::Fifo { priority } => (SCHED_FIFO, *priority),
            Self::RoundRobin { priority } => (SCHED_RR, *priority),
        };

        let param = libc::sched_param {
            sched_priority: priority,
        };

        let result = unsafe { libc::sched_setscheduler(0, policy, &param) };

        if result == -1 {
            let e = std::io::Error::last_os_error();
            eprintln!("could not set scheduler policy: {policy} error: {e}",);
            std::process::exit(1);
        }
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Scheduler::RoundRobin {
            priority: priority(),
        }
    }
}

impl<'de> Deserialize<'de> for Scheduler {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct SchedulerHelper {
            policy: Option<String>,
            niceness: Option<i32>,
            priority: Option<i32>,
        }

        let helper = SchedulerHelper::deserialize(deserializer)?;

        match helper.policy.as_deref() {
            Some("normal") => {
                if helper.priority.is_some() {
                    Err(serde::de::Error::custom(
                        "Cannot specify `priority` for scheduler policy: normal".to_string(),
                    ))
                } else {
                    Ok(Scheduler::Normal {
                        niceness: helper.niceness.unwrap_or_else(niceness),
                    })
                }
            }
            Some("fifo") => {
                if helper.niceness.is_some() {
                    Err(serde::de::Error::custom(
                        "Cannot specify `niceness` for scheduler policy: fifo".to_string(),
                    ))
                } else {
                    Ok(Scheduler::Fifo {
                        priority: helper.priority.unwrap_or_else(priority),
                    })
                }
            }
            Some("round_robin") | None => {
                if helper.niceness.is_some() {
                    Err(serde::de::Error::custom(
                        "Cannot specify `niceness` for scheduler policy: fifo".to_string(),
                    ))
                } else {
                    Ok(Scheduler::RoundRobin {
                        priority: helper.priority.unwrap_or_else(priority),
                    })
                }
            }
            Some(unknown) => Err(serde::de::Error::custom(format!(
                "Unknown scheduler policy: {}",
                unknown
            ))),
        }
    }
}
