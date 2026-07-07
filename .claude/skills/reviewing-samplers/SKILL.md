---
name: reviewing-samplers
description: Use when reviewing a Rezolus sampler change before merge — a new sampler, a change to an existing sampler's probes/refresh/metrics, or core changes that affect samplers; or whenever a sampler's overhead, cadence, or data source is in question.
---

# Reviewing Samplers

## Overview

**Measure, don't assert.** A sampler's overhead is a *number* you produce on real hardware — not an adjective you reason to. `docs/principles.md` is the spec (15 principles + an operational checklist); this skill is the pass that runs it, with one non-negotiable addition: you do not pass a sampler on a predicted cost.

The trap this skill exists to catch: code review reliably flags an *obvious* cost offender (per-refresh `/proc` parsing — principle 15 fires loudly). It does **not** flag a sampler that is a *legitimate* principle-15 exception — sysfs/ioctl/SMI is genuinely the only source. There, no cost alarm fires, so an expensive read sails through as "bounded". That is exactly where you must measure. (`drivehealth` shipped its first cut reasoned "bounded — MET"; measured cost was ~83 ms/refresh — an ATA command per drive.)

## When to use

- Any new sampler, or a change to a sampler's probe / refresh path / metrics.
- Core changes to the sample cycle, exposition, or metric groups.
- A sampler's overhead, cadence, or data source is questioned.

Not for: viewer/recorder/parquet-only changes that don't touch a sampler's refresh path.

## The review

1. **Run the operational checklist in `docs/principles.md`** ("Reviewing or writing a sampler — operational checklist") literally — one yes/no or "justify in a comment" per item, citing the principle. It is the source of truth; do not restate it here.
2. **Measure the overhead (Principle 16) — the load-bearing step.** Produce the per-refresh µs number on real hardware at fleet-representative scale (recipe below). An unmeasured "bounded" / "low" fails the review. Report the number.
3. **Cadence (Principle 17).** Does `refresh()` read a non-mmap, cost-bearing source (sysfs device command, ioctl, SMI/library call, page-table walk)? If so, require: its own bounded re-read interval (configurable), reads dispatched **off** the async worker (`spawn_blocking`/background), and the principle-10 departure documented in the module. A synchronous device read on the sample cycle is a fail.
4. **Data-source justification (Principle 15).** A per-refresh sysfs/procfs parse must be a genuine exception (no BPF/perf hook) *and* commented as such in the module.
5. **Robust to absence.** No device / no permission → the sampler emits zero series and never errors the agent. Linux-only samplers compile a metric-only no-op elsewhere.
6. **Verdict.** GO / NO-GO stated with the *measured numbers* and the specific failing checklist items — never adjectives. The number lands in the effort's journal close-out (see the engineering-journal skill).

## Measurement recipe

Minimal config — only the sampler under review, debug logs, no realtime privilege:

```toml
[general]
listen = "127.0.0.1:4299"
[scheduler]
policy = "normal"     # avoids SCHED_RR / CAP_SYS_NICE for a local run
[log]
level = "debug"
[defaults]
enabled = false
[samplers.<name>]
enabled = true
```

```bash
target/debug/rezolus /tmp/cfg.toml &          # build first
for i in $(seq 5); do curl -s localhost:4299/metrics/json >/dev/null; sleep 0.2; done
grep "<name> sampling latency" <logfile>       # per-sampler µs, this is the number
```

Measure at the **worst case the fleet will hit** — max drive / CPU / process / interface count — not the dev box's trivial count; cost usually scales with it. If you can't reach that scale, say so and bound the extrapolation.

## Red flags — STOP

- About to approve with "looks bounded" / "should be cheap" and **no measured µs number**.
- The refresh reads sysfs/ioctl/SMI/`/proc` but you didn't check cadence (Principle 17).
- Treating a legitimate principle-15 exception as automatically cheap — that is the one case where nothing else will catch the cost.
- Reporting a *target* ("< 100 µs") instead of a *measurement*.

## Common rationalizations

| Excuse | Reality |
|--------|---------|
| "It's O(active keys), so it's bounded." | Bounded is a measured µs number. Produce it. |
| "It's a legit sysfs exception; the cost concern doesn't apply." | The exception is precisely where no alarm fires. Measure it there most of all. |
| "Target is < ~100 µs, that's fine." | A target is a prediction. Report the measurement. |
| "Dev box shows ~0 µs." | Measure at fleet-worst-case device/process count; cost scales with it. |
| "Reads are just small sysfs files." | `drivehealth`'s "small sysfs read" was ~83 ms/refresh. Measure. |
