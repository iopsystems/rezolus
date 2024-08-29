// Provides an async facade for BPF programs

use super::*;
use crate::*;
use core::mem::MaybeUninit;
use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use libbpf_rs::OpenObject;
use metriken::{AtomicHistogram, RwLockHistogram};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

pub struct AsyncBpfBuilder<T: 'static + libbpf_rs::skel::SkelBuilder<'static>> {
    skel: fn() -> T,
    maps: Vec<(&'static str, Vec<u64>)>,
    counters: Vec<(&'static str, Vec<Counter>)>,
    distributions: Vec<(&'static str, &'static RwLockHistogram)>,
    percpu_counters: Vec<(&'static str, Vec<Counter>, Arc<PercpuCounters>)>,
    collected_at: Option<&'static LazyCounter>,
    runtime: Option<&'static LazyCounter>,
    runtime_dist: Option<&'static AtomicHistogram>,
}

pub trait OpenSkelExt {
    /// When called, the SkelBuilder should log instruction counts for each of
    /// the programs within the skeleton. Log level should be debug.
    fn log_prog_instructions(&self);
}

impl<T: 'static> AsyncBpfBuilder<T>
where
    T: libbpf_rs::skel::SkelBuilder<'static>,
    <<T as SkelBuilder<'static>>::Output as OpenSkel<'static>>::Output: OpenSkelExt,
    <<T as SkelBuilder<'static>>::Output as libbpf_rs::skel::OpenSkel<'static>>::Output: GetMap,
{
    pub fn new(skel: fn() -> T) -> Self {
        Self {
            skel,
            maps: Vec::new(),
            counters: Vec::new(),
            distributions: Vec::new(),
            percpu_counters: Vec::new(),
            collected_at: None,
            runtime: None,
            runtime_dist: None,
        }
    }

    pub fn build(self) -> Result<AsyncBpf, ()> {
        let sync = SyncPrimitive::new();
        let sync2 = sync.clone();

        let initialized = Arc::new(AtomicBool::new(false));
        let initialized2 = initialized.clone();

        let thread = std::thread::spawn(move || {
            // storage for the BPF object file
            let open_object: &'static mut MaybeUninit<OpenObject> =
                Box::leak(Box::new(MaybeUninit::uninit()));

            // open and load the BPF program
            let mut skel = match (self.skel)().open(open_object) {
                Ok(s) => match s.load() {
                    Ok(s) => s,
                    Err(e) => {
                        error!("failed to load bpf program: {e}");
                        return;
                    }
                },
                Err(e) => {
                    error!("failed to open bpf builder: {e}");
                    return;
                }
            };

            skel.log_prog_instructions();

            let mut prev = Instant::now();

            // attach the BPF program
            if let Err(e) = skel.attach() {
                error!("failed to attach bpf program: {e}");
                return;
            };

            let mut bpf = crate::common::bpf::BpfBuilder::new(skel);

            for (name, counters) in self.counters.into_iter() {
                bpf = bpf.counters(name, counters);
            }

            for (name, counters, percpu) in self.percpu_counters.into_iter() {
                bpf = bpf.percpu_counters(name, counters, percpu);
            }

            for (name, distribution) in self.distributions.into_iter() {
                bpf = bpf.distribution(name, distribution);
            }

            for (name, map) in self.maps.into_iter() {
                bpf = bpf.map(name, &map);
            }

            let mut bpf = bpf.build();

            initialized.store(true, Ordering::Relaxed);

            loop {
                // wait until we are notified to start
                sync.wait_trigger();

                let now = Instant::now();

                if let Some(collected_at) = self.collected_at {
                    collected_at.set(UnixInstant::EPOCH.elapsed().as_nanos());
                }

                // refresh userspace metrics
                bpf.refresh(now.duration_since(prev));

                // update runtime metadata metrics
                if let (Some(runtime), Some(runtime_dist)) = (self.runtime, self.runtime_dist) {
                    let elapsed = now.elapsed().as_nanos() as u64;

                    runtime.add(elapsed);
                    let _ = runtime_dist.increment(elapsed);
                }

                prev = now;

                // notify that we have finished running
                sync.notify();
            }
        });

        loop {
            if thread.is_finished() {
                return Err(());
            }

            if initialized2.load(Ordering::Relaxed) {
                break;
            }
        }

        Ok(AsyncBpf {
            thread,
            sync: sync2,
        })
    }

    pub fn collected_at(mut self, counter: &'static LazyCounter) -> Self {
        self.collected_at = Some(counter);
        self
    }

    pub fn runtime(
        mut self,
        counter: &'static LazyCounter,
        histogram: &'static AtomicHistogram,
    ) -> Self {
        self.runtime = Some(counter);
        self.runtime_dist = Some(histogram);
        self
    }

    pub fn counters(mut self, name: &'static str, counters: Vec<Counter>) -> Self {
        self.counters.push((name, counters));
        self
    }

    #[allow(dead_code)]
    pub fn percpu_counters(
        mut self,
        name: &'static str,
        counters: Vec<Counter>,
        percpu: Arc<PercpuCounters>,
    ) -> Self {
        self.percpu_counters.push((name, counters, percpu));
        self
    }

    pub fn distribution(mut self, name: &'static str, histogram: &'static RwLockHistogram) -> Self {
        self.distributions.push((name, histogram));
        self
    }

    #[allow(dead_code)]
    pub fn map(mut self, name: &'static str, values: Vec<u64>) -> Self {
        self.maps.push((name, values));
        self
    }
}

pub struct AsyncBpf {
    thread: JoinHandle<()>,
    sync: SyncPrimitive,
}

pub struct AsyncBpfSampler {
    bpf: AsyncBpf,
    interval: AsyncInterval,
}

impl AsyncBpfSampler {
    pub fn new(bpf: AsyncBpf, interval: AsyncInterval) -> Self {
        Self { bpf, interval }
    }

    pub fn is_finished(&self) -> bool {
        self.bpf.thread.is_finished()
    }
}

#[async_trait]
impl AsyncSampler for AsyncBpfSampler {
    async fn sample(&mut self) {
        // wait until it's time to sample
        self.interval.tick().await;

        // check that the thread has not exited
        if self.is_finished() {
            return;
        }

        // notify the thread to start
        self.bpf.sync.trigger();

        // wait for notification that thread has finished
        self.bpf.sync.wait_notify().await;
    }
}
