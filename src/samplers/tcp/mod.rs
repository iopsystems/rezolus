// Copyright 2023 IOP Systems, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

#[cfg(feature = "bpf")]
mod rcv_established;

#[cfg(feature = "bpf")]
mod retransmit_timer;

#[cfg(feature = "bpf")]
mod traffic;

use std::collections::HashSet;
use core::marker::PhantomData;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

#[cfg(feature = "bpf")]
use crate::common::bpf::*;
use crate::config::SamplerConfig;
use crate::samplers::Common;
use crate::*;

#[cfg(feature = "bpf")]
use crate::Statistic as Stat;

mod config;
mod stat;

pub use config::*;
pub use stat::Statistic;

#[cfg(feature = "bpf")]
use rcv_established::*;

#[cfg(feature = "bpf")]
use retransmit_timer::*;

#[cfg(feature = "bpf")]
use traffic::*;

#[allow(dead_code)]
pub struct Tcp<'a> {
    bpf: Option<Arc<Mutex<BpfSamplers<'a>>>>,
    bpf_last: Arc<Mutex<Instant>>,
    common: Common,
    statistics: HashSet<Statistic>,
}

#[async_trait]
impl<'a> Sampler for Tcp<'a> {
    type Statistic = Statistic;
    fn new(common: Common) -> Result<Self, anyhow::Error> {
        let fault_tolerant = common.config.general().fault_tolerant();
        let statistics = common.config().samplers().tcp().statistics().drain(..).collect();

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
        self.common.config().samplers().tcp()
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

impl<'a> Tcp<'a> {
    // checks that bpf is enabled in config and one or more bpf stats enabled
    #[cfg(feature = "bpf")]
    fn bpf_enabled(&self) -> bool {
        if self.sampler_config().bpf() {
            for statistic in &self.statistics {
                if statistic.source() == Source::Distribution {
                    return true;
                }
            }
        }
        false
    }

    fn initialize_bpf(&mut self) -> Result<(), anyhow::Error> {
        if ! self.enabled() || ! self.sampler_config().bpf() {
            return Ok(());
        }

        #[cfg(feature = "bpf")]
        {
            
            let mut bpf_samplers = BpfSamplers::default();

            if self.statistics.contains(&Statistic::Jitter) || self.statistics.contains(&Statistic::SmoothedRoundTripTime) {
                let mut builder = RcvEstablishedSkelBuilder::default();
                let mut skel = builder.open()?.load()?;
                skel.attach()?;

                let bpf = RcvEstablishedBpf {
                    skel,
                    jitter: [0; 496],
                    srtt: [0; 496],
                };

                bpf_samplers.rcv_established = Some(bpf);
            }

            if self.statistics.contains(&Statistic::RetransmissionTimeout) {
                let mut builder = RetransmitTimerSkelBuilder::default();
                let mut skel = builder.open()?.load()?;
                skel.attach()?;

                let bpf = RetransmitTimerBpf {
                    skel,
                    rto: 0,
                };

                bpf_samplers.retransmit_timer = Some(bpf);
            }

            if self.statistics.contains(&Statistic::RxSize) || self.statistics.contains(&Statistic::RxBytes) || self.statistics.contains(&Statistic::RxPackets) ||
                self.statistics.contains(&Statistic::TxSize) || self.statistics.contains(&Statistic::TxBytes) || self.statistics.contains(&Statistic::TxPackets)
            {
                let mut builder = TrafficSkelBuilder::default();
                let mut skel = builder.open()?.load()?;
                skel.attach()?;

                let bpf = TrafficBpf {
                    skel,
                    rx_size: [0; 496],
                    rx_bytes: 0,
                    rx_packets: 0,
                    tx_size: [0; 496],
                    tx_bytes: 0,
                    tx_packets: 0,
                };

                bpf_samplers.traffic = Some(bpf);
            }
            
            self.bpf = Some(Arc::new(Mutex::new(bpf_samplers)));
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

                    if let Some(bpf) = &mut bpf.rcv_established {
                        let mut maps = bpf.skel.maps();

                        let mut current = [0; 8];

                        let sources = vec![
                            (&mut bpf.srtt, maps.srtt(), Statistic::SmoothedRoundTripTime),
                            (&mut bpf.jitter, maps.jitter(), Statistic::Jitter),
                        ];

                        let mut current = [0; 8];

                        for (hist, map, statistic) in sources {
                            for i in 0_u32..496_u32 {
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

                    if let Some(bpf) = &mut bpf.retransmit_timer {
                        let mut maps = bpf.skel.maps();

                        let mut current = [0; 8];

                        if let Ok(Some(c)) = maps.rto().lookup(&0_u32.to_ne_bytes(), libbpf_rs::MapFlags::ANY) {
                            current.copy_from_slice(&c);
                            let current = u64::from_ne_bytes(current);

                            let _ = self.metrics().record_counter(
                                &Statistic::RetransmissionTimeout,
                                time,
                                current,
                            );

                            bpf.rto = current;
                        }
                    }

                    if let Some(bpf) = &mut bpf.traffic {
                        let mut maps = bpf.skel.maps();

                        let mut current = [0; 8];

                        let sources = vec![
                            (&mut bpf.rx_size, maps.rx_size(), Statistic::RxSize),
                            (&mut bpf.tx_size, maps.tx_size(), Statistic::TxSize),
                        ];

                        let mut current = [0; 8];

                        for (hist, map, statistic) in sources {
                            for i in 0_u32..496_u32 {
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

                        if let Ok(Some(c)) = maps.rx_bytes().lookup(&0_u32.to_ne_bytes(), libbpf_rs::MapFlags::ANY) {
                            current.copy_from_slice(&c);
                            let current = u64::from_ne_bytes(current);

                            let _ = self.metrics().record_counter(
                                &Statistic::RxBytes,
                                time,
                                current,
                            );

                            bpf.rx_bytes = current;
                        }

                        if let Ok(Some(c)) = maps.rx_packets().lookup(&0_u32.to_ne_bytes(), libbpf_rs::MapFlags::ANY) {
                            current.copy_from_slice(&c);
                            let current = u64::from_ne_bytes(current);

                            let _ = self.metrics().record_counter(
                                &Statistic::RxPackets,
                                time,
                                current,
                            );

                            bpf.rx_packets = current;
                        }

                        if let Ok(Some(c)) = maps.tx_bytes().lookup(&0_u32.to_ne_bytes(), libbpf_rs::MapFlags::ANY) {
                            current.copy_from_slice(&c);
                            let current = u64::from_ne_bytes(current);

                            let _ = self.metrics().record_counter(
                                &Statistic::TxBytes,
                                time,
                                current,
                            );

                            bpf.tx_bytes = current;
                        }

                        if let Ok(Some(c)) = maps.tx_packets().lookup(&0_u32.to_ne_bytes(), libbpf_rs::MapFlags::ANY) {
                            current.copy_from_slice(&c);
                            let current = u64::from_ne_bytes(current);

                            let _ = self.metrics().record_counter(
                                &Statistic::TxPackets,
                                time,
                                current,
                            );

                            bpf.tx_packets = current;
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
    rcv_established: Option<RcvEstablishedBpf<'a>>,
    retransmit_timer: Option<RetransmitTimerBpf<'a>>,
    traffic: Option<TrafficBpf<'a>>,
}

#[cfg(feature = "bpf")]
pub struct RcvEstablishedBpf<'a> {
    skel: RcvEstablishedSkel<'a>,
    jitter: [u64; 496],
    srtt: [u64; 496],
}

#[cfg(feature = "bpf")]
pub struct RetransmitTimerBpf<'a> {
    skel: RetransmitTimerSkel<'a>,
    rto: u64,
}

#[cfg(feature = "bpf")]
pub struct TrafficBpf<'a> {
    skel: TrafficSkel<'a>,
    rx_size: [u64; 496],
    rx_bytes: u64,
    rx_packets: u64,
    tx_size: [u64; 496],
    tx_bytes: u64,
    tx_packets: u64,
}
