use super::stats::*;
use super::*;
use crate::common::units::{KIBIBYTES, MICROSECONDS, SECONDS};
use crate::common::*;

#[distributed_slice(ASYNC_SAMPLERS)]
fn spawn(config: Arc<Config>, runtime: &Runtime) {
    runtime.spawn(async {
        if let Ok(mut s) = Rusage::new(config) {
            loop {
                s.sample().await;
            }
        }
    });
}

const NAME: &str = "rezolus_rusage";

pub struct Rusage {
    interval: AsyncInterval,
    ru_utime: Counter,
    ru_stime: Counter,
}

impl Rusage {
    pub fn new(config: Arc<Config>) -> Result<Self, ()> {
        // check if sampler should be enabled
        if !config.enabled(NAME) {
            return Err(());
        }

        Ok(Self {
            interval: config.async_interval(NAME),
            ru_utime: Counter::new(&RU_UTIME, None),
            ru_stime: Counter::new(&RU_STIME, None),
        })
    }
}

#[async_trait]
impl AsyncSampler for Rusage {
    async fn sample(&mut self) {
        let (now, elapsed) = self.interval.tick().await;

        METADATA_REZOLUS_RUSAGE_COLLECTED_AT.set(UnixInstant::EPOCH.elapsed().as_nanos());

        self.sample_rusage(elapsed);

        let elapsed = now.elapsed().as_nanos() as u64;
        METADATA_REZOLUS_RUSAGE_RUNTIME.add(elapsed);
        let _ = METADATA_REZOLUS_RUSAGE_RUNTIME_HISTOGRAM.increment(elapsed);
    }
}

impl Rusage {
    fn sample_rusage(&mut self, elapsed: Option<Duration>) {
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
            self.ru_utime.set2(
                elapsed,
                rusage.ru_utime.tv_sec as u64 * SECONDS
                    + rusage.ru_utime.tv_usec as u64 * MICROSECONDS,
            );
            self.ru_stime.set2(
                elapsed,
                rusage.ru_stime.tv_sec as u64 * SECONDS
                    + rusage.ru_stime.tv_usec as u64 * MICROSECONDS,
            );
            RU_MAXRSS.set(rusage.ru_maxrss * KIBIBYTES as i64);
            RU_MINFLT.set(rusage.ru_minflt as u64);
            RU_MAJFLT.set(rusage.ru_majflt as u64);
            RU_INBLOCK.set(rusage.ru_inblock as u64);
            RU_OUBLOCK.set(rusage.ru_oublock as u64);
            RU_NVCSW.set(rusage.ru_nvcsw as u64);
            RU_NIVCSW.set(rusage.ru_nivcsw as u64);
        }
    }
}
