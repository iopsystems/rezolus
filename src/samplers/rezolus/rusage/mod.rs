use super::stats::*;
use super::*;
use crate::common::Nop;

const S: u64 = 1_000_000_000;
const US: u64 = 1_000;
const KB: i64 = 1024;

#[distributed_slice(REZOLUS_SAMPLERS)]
fn init(config: &Config) -> Box<dyn Sampler> {
    if let Ok(rusage) = Rusage::new(config) {
        Box::new(rusage)
    } else {
        Box::new(Nop {})
    }
}

const NAME: &str = "rezolus_rusage";

pub struct Rusage {
    prev: Instant,
    next: Instant,
    interval: Duration,
}

impl Rusage {
    pub fn new(config: &Config) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        let now = Instant::now();

        Ok(Self {
            prev: now,
            next: now,
            interval: config.interval(NAME),
        })
    }
}

impl Sampler for Rusage {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        sample_rusage();

        // determine when to sample next
        let next = self.next + self.interval;

        // it's possible we fell behind
        if next > now {
            // if we didn't, sample at the next planned time
            self.next = next;
        } else {
            // if we did, sample after the interval has elapsed
            self.next = now + self.interval;
        }

        // mark when we last sampled
        self.prev = now;
    }
}

fn sample_rusage() {
    let mut rusage = libc::rusage {
        ru_utime: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        ru_stime: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        ru_maxrss: 0,
        ru_ixrss: 0,
        ru_idrss: 0,
        ru_isrss: 0,
        ru_minflt: 0,
        ru_majflt: 0,
        ru_nswap: 0,
        ru_inblock: 0,
        ru_oublock: 0,
        ru_msgsnd: 0,
        ru_msgrcv: 0,
        ru_nsignals: 0,
        ru_nvcsw: 0,
        ru_nivcsw: 0,
    };

    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut rusage) } == 0 {
        RU_UTIME.set(rusage.ru_utime.tv_sec as u64 * S + rusage.ru_utime.tv_usec as u64 * US);
        RU_STIME.set(rusage.ru_stime.tv_sec as u64 * S + rusage.ru_stime.tv_usec as u64 * US);
        RU_MAXRSS.set(rusage.ru_maxrss * KB);
        RU_IXRSS.set(rusage.ru_ixrss * KB);
        RU_IDRSS.set(rusage.ru_idrss * KB);
        RU_ISRSS.set(rusage.ru_isrss * KB);
        RU_MINFLT.set(rusage.ru_minflt as u64);
        RU_MAJFLT.set(rusage.ru_majflt as u64);
        RU_NSWAP.set(rusage.ru_nswap as u64);
        RU_INBLOCK.set(rusage.ru_inblock as u64);
        RU_OUBLOCK.set(rusage.ru_oublock as u64);
        RU_MSGSND.set(rusage.ru_msgsnd as u64);
        RU_MSGRCV.set(rusage.ru_msgrcv as u64);
        RU_NSIGNALS.set(rusage.ru_nsignals as u64);
        RU_NVCSW.set(rusage.ru_nvcsw as u64);
        RU_NIVCSW.set(rusage.ru_nivcsw as u64);
    }
}
