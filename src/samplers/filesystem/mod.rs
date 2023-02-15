// Copyright 2023 IOP Systems, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

#[cfg(feature = "bpf")]
mod fslat;

#[cfg(feature = "bpf")]
use std::collections::HashSet;
use core::marker::PhantomData;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

#[cfg(feature = "bpf")]
use crate::common::bpf::*;
use crate::config::SamplerConfig;
use crate::samplers::Common;
use crate::*;

mod config;
mod stat;

pub use config::*;
pub use stat::Statistic;

#[cfg(feature = "bpf")]
use fslat::*;

#[allow(dead_code)]
pub struct Filesystem<'a> {
    bpf: Option<Arc<Mutex<BpfSamplers<'a>>>>,
    bpf_last: Arc<Mutex<Instant>>,
    common: Common,
    statistics: Vec<Statistic>,
}

#[async_trait]
impl<'a> Sampler for Filesystem<'a> {
    type Statistic = Statistic;
    fn new(common: Common) -> Result<Self, anyhow::Error> {
        let fault_tolerant = common.config.general().fault_tolerant();
        let statistics = common.config().samplers().filesystem().statistics();

        #[allow(unused_mut)]
        let mut sampler = Self {
            bpf: None,
            bpf_last: Arc::new(Mutex::new(Instant::now())),
            common,
            statistics,
        };

        if let Err(e) = sampler.initialize_bpf() {
            error!("failed to initialize bpf: {}", e);
            if !fault_tolerant {
                return Err(e);
            }
        }

        if sampler.sampler_config().enabled() {
            sampler.register();
        }

        Ok(sampler)
    }

    fn common(&self) -> &Common {
        &self.common
    }

    fn common_mut(&mut self) -> &mut Common {
        &mut self.common
    }

    fn sampler_config(&self) -> &dyn SamplerConfig<Statistic = Self::Statistic> {
        self.common.config().samplers().filesystem()
    }

    async fn sample(&mut self) -> Result<(), std::io::Error> {
        if let Some(ref mut delay) = self.delay() {
            delay.tick().await;
        }

        if !self.sampler_config().enabled() {
            return Ok(());
        }

        debug!("sampling");

        // sample bpf
        #[cfg(feature = "bpf")]
        self.map_result(self.sample_bpf())?;

        Ok(())
    }
}

impl<'a> Filesystem<'a> {
    // checks that bpf is enabled in config and one or more bpf stats enabled
    #[cfg(feature = "bpf")]
    fn bpf_enabled(&self) -> bool {
        if self.sampler_config().bpf() {
            for statistic in &self.statistics {
                match statistic {
                    Statistic::ReadLatency | Statistic::WriteLatency | Statistic::OpenLatency | Statistic::FsyncLatency => {
                        return true;
                    }
                    _ => {}
                }
            }
        }
        false
    }

    fn initialize_bpf(&mut self) -> Result<(), anyhow::Error> {
        #[cfg(feature = "bpf")]
        {
            if self.enabled() && self.bpf_enabled() {
                debug!("initializing bpf");

                let mut bpf = BpfSamplers::default();

                let mut builder = FslatSkelBuilder::default();
                // builder.obj_builder.debug(true);

                let mut fslat = builder.open()?.load()?;
                fslat.attach()?;

                let fslat = Fslat {
                    skel: fslat,
                    read_latency: [0; 761],
                    write_latency: [0; 761],
                    open_latency: [0; 761],
                    fsync_latency: [0; 761],
                };

                bpf.fslat = Some(fslat);
                self.bpf = Some(Arc::new(Mutex::new(bpf)));
            }
        }

        Ok(())
    }

    #[cfg(feature = "bpf")]
    fn sample_bpf(&self) -> Result<(), std::io::Error> {
        // sample bpf
        {
            if self.bpf_last.lock().unwrap().elapsed()
                >= Duration::from_secs(1)
            {
                if let Some(ref bpf) = self.bpf {
                    let mut bpf = bpf.lock().unwrap();
                    let time = Instant::now();

                    if let Some(fslat) = &mut bpf.fslat {
                        let mut maps = fslat.skel.maps();

                        let mut current = [0; 8];

                        let sources = vec![
                            (&mut fslat.read_latency, maps.read_latency(), Statistic::ReadLatency),
                            (&mut fslat.write_latency, maps.write_latency(), Statistic::WriteLatency),
                            (&mut fslat.open_latency, maps.open_latency(), Statistic::OpenLatency),
                            (&mut fslat.fsync_latency, maps.fsync_latency(), Statistic::FsyncLatency),
                        ];

                        let mut current = [0; 8];

                        for (hist, map, statistic) in sources {
                            for i in 0_u32..731_u32 {
                                match map.lookup(&i.to_ne_bytes(), libbpf_rs::MapFlags::ANY) {
                                    Ok(Some(c)) => {
                                        // convert the index to a usize, as we use it a few
                                        // times to index into slices
                                        let i = i as usize;

                                        // convert bytes to the current count of the bucket
                                        current.copy_from_slice(&c);
                                        let current = u64::from_ne_bytes(current);

                                        // calculate the delta from previous count
                                        let delta = current.wrapping_sub(hist[i]);

                                        // update the previous count
                                        hist[i] = current;

                                        // update the heatmap
                                        if delta > 0 {
                                            let value = key_to_value(i as u64);
                                            let _ = self.metrics().record_bucket(
                                                &statistic,
                                                time,
                                                value,
                                                delta as u32,
                                            );
                                        }
                                    }
                                    _ => { }
                                }
                            }
                        }
                        
                    }
                }
                *self.bpf_last.lock().unwrap() = Instant::now();
            }
        }

        Ok(())
    }
}

#[cfg(not(feature = "bpf"))]
pub struct BpfSamplers<'a> {
    // used to mark the placeholder type with the appropriate lifetime
    _lifetime: PhantomData<&'a ()>
}

#[cfg(feature = "bpf")]
#[derive(Default)]
pub struct BpfSamplers<'a> {
    fslat: Option<Fslat<'a>>,
}

#[cfg(feature = "bpf")]
pub struct Fslat<'a> {
    skel: FslatSkel<'a>,
    read_latency: [u64; 761],
    write_latency: [u64; 761],
    open_latency: [u64; 761],
    fsync_latency: [u64; 761],
}
