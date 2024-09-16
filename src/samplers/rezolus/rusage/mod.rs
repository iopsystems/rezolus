const NAME: &str = "rezolus_rusage";

use crate::common::*;
use crate::samplers::rezolus::stats::*;
use crate::*;

#[distributed_slice(SAMPLERS)]
fn init(config: Arc<Config>) -> SamplerResult {
    if !config.enabled(NAME) {
        return Ok(None);
    }

    Ok(Some(Box::new(Rusage {})))
}

pub struct Rusage {}

#[async_trait]
impl Sampler for Rusage {
    async fn refresh(&self) {
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
            RU_UTIME.set(
                rusage.ru_utime.tv_sec as u64 * SECONDS
                    + rusage.ru_utime.tv_usec as u64 * MICROSECONDS,
            );
            RU_STIME.set(
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
