# Shared blocking sampler admission

This follows the worker-helper suggestion from the local review of PR #1537.
It is stacked on `feat/cgroup-memory-accounting` at `615695c8`. The memory
feature and its other review follow-ups remain in the parent PR.

## Behavior

`BlockingWorker` in `src/agent/samplers/blocking.rs` owns admission and
cadence for cgroup memory, Linux hardware sensors, and Linux/macOS drive
health. Refresh tries a short scheduling mutex and immediately returns if
it is contended, a worker is already queued/running, or the interval has not
elapsed. Sampler state is cloned only after admission. The resulting permit
owns the worker slot until the blocking closure is dropped, including
cancellation before execution and unwinding.

The existing cadence choices are explicit: sensors and drive health measure
from dispatch, and cgroup memory measures from completion. Rejected refreshes
never advance either clock. This fixes drive health's previous behavior where
a refresh during a long read could consume the next interval without actually
dispatching. Hardware sensors retain their warning once a sweep exceeds three
intervals; it resets on the next admitted worker. Sensor rediscovery retains
its independent 60-second throttle.

All workers catch unwinding panics, log the sampler name, release their permit,
and wait a full interval from failure before another admission. The shared
state-lock recovery retains existing slot ownership and clears poisoning;
it does not roll back partial writes. The binary's existing global panic hook
exits the process before unwinding. This refactor does not change that policy
and does not promise process survival after a production panic. Recovery tests
exercise the unwinding behavior under the test harness's panic hook.

Metric reads, publication, acquisition windows and ordinary read-error handling
remain in their samplers. There is no agent timer, added device read, changed
default interval, or new overlapping writer (principles 10, 17 and 18).

## Cost and validation

A standalone release-mode harness imports the actual helper module and measures
five batches of one million calls to `try_acquire`, with `black_box` on the
worker and result. On this x86_64 Intel i5-13500H host, Rust 1.97.1:

| Admission rejected because | ns per refresh |
| --- | ---: |
| Dispatch-based interval has not elapsed | 30.5–30.9 |
| Completion-based interval has not elapsed | 30.5–30.8 |
| Worker remains in flight | 30.9–31.0 |

The harness is `/tmp/rezolus-worker-bench` and its output is
`/tmp/rezolus-worker-bench.log`. These are admission-only measurements, not
end-to-end sampler or cgroupfs overhead, and not a comparison with the parent.
Actual device-read code and the number of reads per sweep are unchanged.

Shared tests cover both cadence choices at interval boundaries, concurrent
callers, contention without waiting, stuck-warning reset, progress of a
single-thread async runtime during a blocking read, unwinding with poisoned
state and delayed retry, and cancellation of a queued task. The sensor fixture
test still exercises a real background sweep and suppression of a second read.
MacOS hardware execution is unavailable on this Linux host; the repository's
macOS CI must validate the drive-health integration there.

Validation on the stacked branch: the full root unit suite passed 1136 tests
(12 ignored), including the snapshot contract and localhost stream/recorder
tests. The targeted sampler run passed 48 tests (four hardware/capacity tests
ignored). `cargo clippy --offline -p rezolus --all-targets` completed with only
the existing `manual_range_contains` warning in `src/mcp/link.rs`. Formatting
and diff whitespace checks passed. The stack advances the development version
from the parent's `6.0.0-alpha.65` to `6.0.0-alpha.66`.
