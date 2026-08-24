//! Occupy a known number of PMU counters, so PMU-scarcity behaviour can be
//! measured instead of argued about.
//!
//! The agent opens more pinned hardware events than the PMU has counters
//! (rezolus#1053). The excess open successfully, never get scheduled, and read
//! a frozen value forever. Reproducing that deliberately needs a way to leave
//! the PMU with an exact number of free counters — otherwise every claim about
//! what happens under scarcity rests on argument rather than a measurement.
//!
//! Two modes:
//!
//! ```text
//! cargo run --example pmu_hog -- probe [--cpu N]
//! cargo run --example pmu_hog -- hold <count> [--cpu N] [--seconds S]
//!
//! `--cpu N` restricts either mode to one CPU; `hold` covers every online CPU
//! without it.
//! ```
//!
//! `probe` reports how many pinned hardware counters can be placed on a CPU
//! *right now* — availability, not the hardware maximum, so whatever else is
//! running is already accounted for.
//!
//! `hold` occupies `count` counters PER CPU and stays up, leaving the rest
//! free. Every online CPU by default, because a CPU-wide event the agent opens
//! lands on all of them — hogging one CPU would leave the experiment measuring
//! the other thirty-one. Pair the two to set up an exact scarcity: probe to
//! learn the budget, hold `budget - k`, then start the thing under test with
//! `k` counters left on every CPU.
//!
//! Both need permission to open CPU-wide events: run as root, or set
//! `kernel.perf_event_paranoid <= 0`.
//!
//! Linux only — `perf_event_open` does not exist elsewhere.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("pmu_hog requires perf_event_open, which is Linux-only");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}

#[cfg(target_os = "linux")]
mod linux {
    use perf_event::events::Hardware;
    use perf_event::{Builder, Counter, ReadFormat};
    use std::time::{Duration, Instant};

    /// A counter that opened but was never scheduled reads `time_running == 0`.
    /// That is the whole signal: `perf_event_open` succeeding says nothing
    /// about whether the event got a counter, which is exactly why
    /// over-subscription is silent in the first place.
    fn is_scheduled(counter: &mut Counter) -> bool {
        counter
            .read_full()
            .ok()
            .and_then(|r| r.time_running())
            .is_some_and(|running| !running.is_zero())
    }

    /// Open one pinned, CPU-wide `instructions` counter on `cpu`.
    ///
    /// Pinned because that is what the agent does, and pinned is what makes
    /// scarcity a hard failure rather than time-sharing: the kernel places
    /// pinned events first-come-first-served and leaves the losers enabled but
    /// never scheduled.
    fn open_one(cpu: usize) -> std::io::Result<Counter> {
        let mut builder = Builder::new(Hardware::INSTRUCTIONS);
        builder
            .one_cpu(cpu)
            .any_pid()
            .exclude_hv(false)
            .exclude_kernel(false)
            .pinned(true)
            .read_format(ReadFormat::TOTAL_TIME_ENABLED | ReadFormat::TOTAL_TIME_RUNNING);
        builder.build()
    }

    /// Open counters on `cpu` until one fails to open or opens without being
    /// scheduled. Returns the ones that are actually counting.
    ///
    /// Stopping at the first unscheduled counter is the point: `open_one`
    /// keeps succeeding well past the hardware limit, so counting successful
    /// opens would measure nothing at all.
    fn take_while_scheduled(cpu: usize, limit: usize) -> Vec<Counter> {
        let mut held = Vec::new();
        for _ in 0..limit {
            let Ok(mut counter) = open_one(cpu) else {
                break;
            };
            if counter.enable().is_err() || !is_scheduled(&mut counter) {
                break;
            }
            held.push(counter);
        }
        held
    }

    /// The online CPUs, from `/sys/devices/system/cpu/online` (e.g. `0-31`,
    /// or `0-3,8-11` on a machine with some offline).
    ///
    /// Not `available_parallelism`: that answers what THIS process may run on,
    /// which affinity can narrow. A CPU left un-hogged because it was outside
    /// our affinity mask would silently keep its counters free and invalidate
    /// whatever the hog was set up to measure.
    fn online_cpus() -> Vec<usize> {
        let Ok(text) = std::fs::read_to_string("/sys/devices/system/cpu/online") else {
            return vec![0];
        };
        let mut cpus = Vec::new();
        for part in text.trim().split(',') {
            match part.split_once('-') {
                Some((lo, hi)) => {
                    if let (Ok(lo), Ok(hi)) = (lo.parse::<usize>(), hi.parse::<usize>()) {
                        cpus.extend(lo..=hi);
                    }
                }
                None => {
                    if let Ok(n) = part.parse::<usize>() {
                        cpus.push(n);
                    }
                }
            }
        }
        if cpus.is_empty() {
            vec![0]
        } else {
            cpus
        }
    }

    /// Generous ceiling on one CPU's general-purpose counters — the loop stops
    /// at the real limit long before this, which only bounds a pathological
    /// case.
    const MAX_PROBE: usize = 64;

    fn usage() -> ! {
        eprintln!(
            "usage:\n  \
             pmu_hog probe [--cpu N]\n  \
             pmu_hog hold <count> [--cpu N] [--seconds S]"
        );
        std::process::exit(2)
    }

    fn flag(args: &[String], name: &str) -> Option<usize> {
        let i = args.iter().position(|a| a == name)?;
        args.get(i + 1)?.parse().ok()
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let Some(mode) = args.first().map(String::as_str) else {
            usage()
        };
        let cpu = flag(&args, "--cpu").unwrap_or(0);

        match mode {
            "probe" => {
                let held = take_while_scheduled(cpu, MAX_PROBE);
                println!(
                    "cpu{cpu}: {} pinned hardware counters available",
                    held.len()
                );
                if held.is_empty() {
                    println!("  (none — something else already holds every counter on this CPU)");
                }
            }
            "hold" => {
                let Some(want) = args.get(1).and_then(|a| a.parse::<usize>().ok()) else {
                    usage()
                };
                let seconds = flag(&args, "--seconds");

                let targets: Vec<usize> = if args.iter().any(|a| a == "--cpu") {
                    vec![cpu]
                } else {
                    online_cpus()
                };

                let mut held = Vec::new();
                let mut short = Vec::new();
                for &cpu in &targets {
                    let got = take_while_scheduled(cpu, want);
                    if got.len() < want {
                        short.push((cpu, got.len()));
                    }
                    held.extend(got);
                }

                // Report what was actually taken, not what was asked for: on a
                // busy PMU these differ, and a hog that quietly took fewer
                // counters than requested would invalidate whatever experiment
                // it was set up for.
                println!(
                    "holding {} counters: {want} per cpu across {} cpu(s)",
                    held.len(),
                    targets.len()
                );
                if !short.is_empty() {
                    println!(
                        "  SHORT on {} cpu(s), so the free count is NOT uniform:",
                        short.len()
                    );
                    for (cpu, got) in short.iter().take(8) {
                        println!("    cpu{cpu}: {got} of {want}");
                    }
                }

                match seconds {
                    Some(s) => {
                        let until = Instant::now() + Duration::from_secs(s as u64);
                        while Instant::now() < until {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                    None => {
                        println!("  holding until killed (ctrl-c)");
                        loop {
                            std::thread::sleep(Duration::from_millis(500));
                        }
                    }
                }
                // `held` drops here, closing the fds and freeing the counters.
                drop(held);
            }
            _ => usage(),
        }
    }
}
