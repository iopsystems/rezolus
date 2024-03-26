use super::stats::*;
use super::*;
use crate::common::Counter;
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
    ru_utime: Counter,
    ru_stime: Counter,
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
            ru_utime: Counter::new(&RU_UTIME, Some(&RU_UTIME_HISTOGRAM)),
            ru_stime: Counter::new(&RU_STIME, Some(&RU_STIME_HISTOGRAM)),
        })
    }
}

impl Sampler for Rusage {
    fn sample(&mut self) {
        let now = Instant::now();

        if now < self.next {
            return;
        }

        let elapsed = (now - self.prev).as_secs_f64();

        self.sample_rusage(elapsed);

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

impl Rusage {
    fn sample_rusage(&mut self, elapsed: f64) {
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
            self.ru_utime.set(
                elapsed,
                rusage.ru_utime.tv_sec as u64 * S + rusage.ru_utime.tv_usec as u64 * US,
            );
            self.ru_stime.set(
                elapsed,
                rusage.ru_stime.tv_sec as u64 * S + rusage.ru_stime.tv_usec as u64 * US,
            );
            RU_MAXRSS.set(rusage.ru_maxrss * KB);
            RU_MINFLT.set(rusage.ru_minflt as u64);
            RU_MAJFLT.set(rusage.ru_majflt as u64);
            RU_INBLOCK.set(rusage.ru_inblock as u64);
            RU_OUBLOCK.set(rusage.ru_oublock as u64);
            RU_NVCSW.set(rusage.ru_nvcsw as u64);
            RU_NIVCSW.set(rusage.ru_nivcsw as u64);
        }
    }
}
