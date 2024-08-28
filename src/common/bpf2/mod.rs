use crate::*;
use super::*;
// use crate::common::bpf::BpfBuilder;
use core::mem::MaybeUninit;
use libbpf_rs::skel::SkelBuilder;
use libbpf_rs::skel::OpenSkel;
use libbpf_rs::OpenObject;
pub use crate::common::bpf::GetMap;
use libbpf_rs::skel::Skel;
use core::marker::PhantomData;
use crate::common::bpf::PercpuCounters;
use metriken::RwLockHistogram;

pub struct BpfBuilder<T: 'static + libbpf_rs::skel::SkelBuilder<'static>> {
    _skel: PhantomData<T>,
	inner: crate::common::bpf::BpfBuilder<<<T as SkelBuilder<'static>>::Output as libbpf_rs::skel::OpenSkel<'static>>::Output>,
}

pub trait OpenSkelExt {
	/// When called, the SkelBuilder should log instruction counts for each of
	/// the programs within the skeleton. Log level should be debug.
	fn log_prog_instructions(&self);
}

impl<T: 'static> BpfBuilder<T>
where 
    T: libbpf_rs::skel::SkelBuilder<'static>,
    <<T as SkelBuilder<'static>>::Output as OpenSkel<'static>>::Output: OpenSkelExt,
    <<T as SkelBuilder<'static>>::Output as libbpf_rs::skel::OpenSkel<'static>>::Output: GetMap,
{
	pub fn new(skel_builder: T) -> Result<Self, ()> {
		// storage for the BPF object file
        let open_object: &'static mut MaybeUninit<OpenObject> =
            Box::leak(Box::new(MaybeUninit::uninit()));

        let mut skel = match skel_builder.open(open_object) {
            Ok(s) => match s.load() {
                Ok(s) => s,
                Err(e) => {
                    error!("failed to load bpf program: {e}");
                    return Err(());
                }
            },
            Err(e) => {
                error!("failed to open bpf builder: {e}");
                return Err(());
            }
        };

        skel.log_prog_instructions();

       	// attach the BPF program
        if let Err(e) = skel.attach() {
            error!("failed to attach bpf program: {e}");
            return Err(());
        };

        // wrap the BPF program and define BPF maps
        Ok(Self {
            _skel: PhantomData,
        	inner: crate::common::bpf::BpfBuilder::new(skel),
        })
	}

    pub fn build(self) -> Bpf<<<T as SkelBuilder<'static>>::Output as OpenSkel<'static>>::Output> {
        Bpf { inner: self.inner.build() }
    }

    pub fn counters(mut self, name: &str, counters: Vec<Counter>) -> Self {
        self.inner = self.inner.counters(name, counters);
        self
    }

    pub fn percpu_counters(
        mut self,
        name: &str,
        counters: Vec<Counter>,
        percpu: Arc<PercpuCounters>,
    ) -> Self {
        self.inner = self.inner.percpu_counters(name, counters, percpu);
        self
    }

    pub fn distribution(mut self, name: &str, histogram: &'static RwLockHistogram) -> Self {
        self.inner = self.inner.distribution(name, histogram);
        self
    }

    pub fn map(mut self, name: &str, values: &[u64]) -> Self {
        self.inner = self.inner.map(name, values);
        self
    }
}

pub struct Bpf<T: 'static> {
    inner: crate::common::bpf::Bpf<T>,
}

impl<T: 'static + GetMap> Bpf<T> {
    pub fn refresh(&mut self, elapsed: Duration) {
        self.inner.refresh(elapsed)
    }
}

