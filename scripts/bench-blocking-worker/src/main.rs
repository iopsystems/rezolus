//! Run with `cargo run --release --locked --manifest-path
//! scripts/bench-blocking-worker/Cargo.toml` from the repository root.

use log::{error, warn};

#[allow(dead_code)]
#[path = "../../../src/agent/samplers/blocking.rs"]
mod blocking;

use blocking::{BlockingWorker, Cadence};
use std::hint::black_box;
use std::time::{Duration, Instant};

fn measure(name: &str, worker: &BlockingWorker) {
    const CALLS: usize = 1_000_000;
    for _ in 0..5 {
        let start = Instant::now();
        for _ in 0..CALLS {
            black_box(black_box(worker).try_acquire());
        }
        println!(
            "{name}: {:.1} ns/refresh",
            start.elapsed().as_nanos() as f64 / CALLS as f64
        );
    }
}

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for (name, cadence) in [
        ("dispatch throttled", Cadence::FromDispatch),
        ("completion throttled", Cadence::FromCompletion),
    ] {
        let worker = BlockingWorker::new("bench", Duration::from_secs(60), cadence);
        // Complete actual work: dropping an unspawned permit consumes no interval.
        runtime.block_on(async { worker.try_acquire().unwrap().spawn(|| {}).await.unwrap() });
        measure(name, &worker);
    }
    let worker = BlockingWorker::new("bench", Duration::from_secs(60), Cadence::FromDispatch);
    let _permit = worker.try_acquire().unwrap();
    measure("in-flight", &worker);
}
