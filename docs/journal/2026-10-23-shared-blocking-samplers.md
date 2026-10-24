# Shared blocking sampler admission

This follows the worker-helper suggestion from the local review of PR #1537.
It is stacked on `feat/cgroup-memory-accounting` at `37f3e29e`. The memory
feature and its other review follow-ups remain in the parent PR.

## Behavior

`BlockingWorker` in `src/agent/samplers/blocking.rs` owns admission and
cadence for cgroup memory, Linux hardware sensors, and Linux/macOS drive
health. Refresh tries a short scheduling mutex and immediately returns if
it is contended, a worker is already queued/running, or the interval has not
elapsed. Sampler state is cloned only after admission. The resulting permit
owns the worker slot until the blocking closure is dropped, including
cancellation before execution and unwinding. Cancellation before the closure
starts restores the previous cadence anchor, so no interval is consumed without
a read. A task that has started cannot be cancelled through its join handle.

Hardware sensors measure from dispatch. Cgroup memory and Linux/macOS drive
health measure from completion. The review of `bf36bf2e` caught that preserving
drive health's nominal dispatch cadence would permit back-to-back device commands
after an overrun. Completion-based spacing guarantees an idle interval: a 90s
sweep with a 60s interval next becomes eligible at 150s. Rejected refreshes
never advance either clock. Hardware sensors retain their warning once a sweep
exceeds three intervals; it resets on the next admitted worker. Sensor rediscovery retains
its independent 60-second throttle.

All workers catch unwinding panics, log the sampler name, release their permit,
and wait a full interval from failure before another admission. The shared
state-lock recovery logs the sampler name, retains existing slot ownership and
clears poisoning; it does not roll back partial writes. The binary's existing global panic hook
exits the process before unwinding. This refactor does not change that policy
and does not promise process survival after a production panic. Recovery tests
exercise the unwinding behavior under the test harness's panic hook.

Metric reads, publication, acquisition windows and ordinary read-error handling
remain in their samplers. There is no agent timer, added device read, changed
default interval setting, or new overlapping writer (principles 10, 17 and 18).
Drive health's spacing now includes the duration of the preceding sweep.
Admission racing the brief completion lock may skip a consumer tick instead of
blocking; completion never holds that lock across device work.

The remaining gates in filesystem, Intel GPU, slabinfo and XFS stats are deferred
to a separate migration. This PR remains scoped to the three samplers named in
the original review follow-up; their differing state and publication lifecycles
need their own validation before adopting the helper.

## Cost and validation

A standalone release-mode harness imports the actual helper module and measures
five batches of one million calls to `try_acquire`, with `black_box` on the
worker and result. On this x86_64 Intel i5-13500H host, Rust 1.97.1:

| Admission rejected because | ns per refresh |
| --- | ---: |
| Dispatch-based interval has not elapsed | 30.2–30.5 |
| Completion-based interval has not elapsed | 30.2–30.3 |
| Worker remains in flight | 30.3–30.6 |

The harness and its dependency lockfile are checked in under
`scripts/bench-blocking-worker`. Reproduce from the repository root with:

```sh
cargo run --release --locked --manifest-path scripts/bench-blocking-worker/Cargo.toml
```

These figures were remeasured after the review fixes; the original `bf36bf2e`
measurement was 30.5–31.0 ns. These are admission-only measurements, not
end-to-end sampler or cgroupfs overhead, and not a comparison with the parent.
Actual device-read code and the number of reads per sweep are unchanged.

Shared tests cover both cadence choices at interval boundaries, concurrent
callers, contention without waiting, stuck-warning reset, progress of a
single-thread async runtime during a blocking read, unwinding with poisoned
state and delayed retry, and cancellation of a queued task with a nonzero
interval. The simulated cadence tests move time forward from `Instant::now()`;
they do not subtract an assumed uptime or sleep through long device intervals.
The sensor fixture test still exercises a real background sweep and suppression
of a second read.
macOS hardware execution is unavailable on this Linux host; the repository's
macOS CI must validate the drive-health integration there.

Validation before the review fixes: the full root unit suite passed 1136 tests
(12 ignored), including the snapshot contract and localhost stream/recorder
tests. The targeted sampler run passed 48 tests (four hardware/capacity tests
ignored). `cargo clippy --offline -p rezolus --all-targets` completed with only
the existing `manual_range_contains` warning in `src/mcp/link.rs`. Formatting
and diff whitespace checks passed. The stack advances the development version
from the parent's `6.0.0-alpha.65` to `6.0.0-alpha.66`.

After the review fixes, 1138 root unit tests passed (12 ignored), and the
targeted sampler run passed 50 (four ignored). The `hindsight_dump` integration
target passed 13 tests before the SIGHUP capture test hit the local `/tmp` disk
quota; that test passed when rerun with temporary files on the workspace disk.
The checked-in benchmark and its Clippy check passed. Root Clippy across all
targets again reported only the existing `manual_range_contains` warning.
Formatting and diff whitespace checks passed.

After PR #1543 merged as `d5337d18`, the parent integrated main and advanced
to alpha.66. This stack now advances to `6.0.0-alpha.67`. Integrating the new
parent changes CI workflows, documentation and the version only; sampler and
test sources remain identical to reviewed commit `79f26788`.
