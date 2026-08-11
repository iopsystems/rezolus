# Label-Generation Harness

Runbook for `labels/` — the controlled-experiment harness that produces
recordings whose bottleneck is *known by construction*, so candidate
assessments (from a frontier model, and eventually a fine-tune) can be
verified against ground truth rather than vibes-checked. Full design:
`docs/superpowers/specs/2026-08-10-label-harness-design.md`.

This is sub-project #3 of the recording-assessment pipeline. Phases 1-3
delivered the record format (`OverviewRecord` v2), the extraction library,
and the `rezolus mcp extract-features` front door. This harness adds truth.
Distillation of verified pairs into training labels: see `distill.md`.

## Layout

```
labels/
  README.md                  # this file
  specs/
    cpu_saturation.toml       # systemslab experiment spec
    scheduler_contention.toml
  ground_truth/
    cpu_saturation.json       # GroundTruth document, co-versioned with the spec
    scheduler_contention.json
  exemplars/
    cpu_saturation.assessment.json        # hand-written full Assessment per class
    scheduler_contention.assessment.json
  corpus/                     # gitignored; fetched recordings + paired labels
    <class>/<experiment-id>/recording.rez
    <class>/<experiment-id>/ground_truth.json
    <class>/<experiment-id>/record.json    # extracted OverviewRecord (step 1 below)
```

`GroundTruth` (`src/analysis/ground_truth.rs`) is the machine-checkable
schema: bottleneck class, subsystem/mechanism/direction, the induced-phase
window in seconds, and a list of `ExpectedSignal { metric, check }` pairs
that `GroundTruth::verify(&OverviewRecord)` mechanically evaluates. `verify`
accumulates *all* failures (`Result<(), Vec<String>>`) rather than failing
fast, so a run can be triaged in one pass.

## MCP flow: submit, fetch, verify

The corpus is built agent-driven via the systemslab MCP tools — no bespoke
submission/fetch tooling in v1.

1. **Validate the spec.** `validate_spec` on `labels/specs/<class>.toml`.
   Fix and re-validate until it returns `valid: true`.
2. **Submit.** `submit_experiment` with the validated spec.
3. **Poll.** `get_experiment` until the run completes.
4. **Fetch.** `download_artifact` for both `recording.rez` and
   `ground_truth.json`, into
   `labels/corpus/<class>/<experiment-id>/recording.rez` and
   `labels/corpus/<class>/<experiment-id>/ground_truth.json`
   (create the directory as needed — `corpus/` is gitignored).

## Verifying a fetched pair

1. Extract features from the recording:

   ```bash
   target/release/rezolus mcp extract-features labels/corpus/<class>/<experiment-id>/recording.rez \
     > labels/corpus/<class>/<experiment-id>/record.json
   ```

2. Evaluate `GroundTruth::verify` against the extracted record. There is no
   `verify-ground-truth` front door in v1 (named Phase-5 item, not YAGNI'd
   away, just not built yet) — instead, `src/analysis/ground_truth.rs` has
   a committed `#[ignore]`d test, `verify_corpus_pair`, that does exactly
   this: it deserializes `GroundTruth` and `OverviewRecord` from two
   env-var-pointed paths and calls `verify()`, panicking with all
   accumulated failures if it fails. Run it against a fetched pair:

   ```bash
   CORPUS_RECORD=labels/corpus/<class>/<experiment-id>/record.json \
   CORPUS_TRUTH=labels/corpus/<class>/<experiment-id>/ground_truth.json \
   cargo test verify_corpus_pair -- --ignored --nocapture
   ```

   As a library-level sanity check that the schema and the committed
   ground-truth documents themselves are well-formed (this does not touch a
   real recording), run:

   ```bash
   cargo test analysis::ground_truth
   ```

3. **Only verified pairs stay in the corpus.** If `verify()` returns
   `Err(errs)`, either the experiment didn't induce the expected bottleneck
   (investigate/retry) or the ground-truth document needs calibration (see
   below) — do not keep an unverified pair in `corpus/`. Record the outcome
   in the calibration log.

   A healthy run produces a ~250s recording (60s baseline + 120s induce +
   60s cooldown + ~10s recorder lead-in); `verify()` rejects recordings
   shorter than the induced window's end (180s) with a duration error, so a
   truncated recording fails loudly rather than silently passing on
   whatever data happened to land in range.

## Calibration log

| date | class | experiment id | verify outcome | notes |
|------|-------|----------------|-----------------|-------|
| 2026-08-11 | cpu_saturation | 019fef20-8fd1-7197-2900-c861fa1295d3 | PASS (first verified run) | cpu_usage Increase shifts at indices 45/75 (rate-window ramp-in splits the step in two), Decrease at 165/195 (ramp-out; 165 is in-window but only Increase counts). Uncertainty bands present (band/signal ≈ 0.005). Host: hv02 (trixie), recorder 5.17.1-alpha.8 extracted from the release deb, host agent 5.17.0. |
| 2026-08-11 | scheduler_contention | 019fef20-c9e8-71ed-adc2-a4fd3408c50f | PASS (first verified run) | scheduler_runqueue_latency:p99 series p99/p50 ≈ 768× (vs ≈256× under pure cpu saturation — see caution below). cpu_usage Increase shifts at 45/75. Signal lists verified correct as authored; no ground-truth changes needed. |
| 2026-08-11 | (both) | 019feef7-5fa5…/019feef7-9aa6… | FAIL (provisioning) | Host stable rezolus 5.17.0 predates `record --url`/.rez → tagged v5.17.1-alpha.8 prerelease. Second attempt (019fef17-9a92…/019fef17-d618…) failed on `dpkg -i` (job shell is not root) → specs now extract the deb (`dpkg-deb -x`) and run the recorder from /tmp. |

Calibration findings worth keeping:
- **Acquisition windows come from the recorder, not the agent** — recordings
  made by the 5.17.1-alpha recorder against a 5.17.0 agent still carry
  uncertainty bands (the earlier in-spec caveat saying otherwise was wrong
  and has been superseded by this note).
- **`ElevatedInWindow` on runqueue latency does not discriminate between the
  two v1 classes** — pure CPU saturation also elevates the p99/p50 ratio far
  past the 2× threshold. Class separation comes from the experiment's
  construction, not from that check; distillation must not treat it as a
  class discriminator.
- **The rate()-window ramp splits a step change into two Increase shifts**
  (~indices 45 and 75 for a step at t=60) and produces pre-step anomaly
  flags; window offsets in ground truths should stay padded, never
  boundary-exact.

## Metric-name calibration caveat

The `expected_signals` metric names in `labels/ground_truth/*.json` (e.g.
`cpu_usage`, `scheduler_runqueue_latency:p99`) are written from the sampler
naming conventions, but are not yet confirmed against a real extracted
record from these exact experiments. The first end-to-end acceptance run
per class is where that calibration happens: fetch the pair, extract
features, diff the expected metric names against what the record actually
emits (accounting for the extraction layer's `:pNN` histogram-quantile
suffixing), and update `labels/ground_truth/*.json` (and the corresponding
`write_file` step embedded in `labels/specs/*.toml`, which must stay in
sync by hand) to match reality. This is a deliberate calibration step, not
drift — log it in the calibration table above.

## Exemplars

`labels/exemplars/<class>.assessment.json` are hand-written, complete
`Assessment` documents (schema: `src/analysis/assessment.rs`) — one per v1
bottleneck class. They exist so the assessment schema has a concrete,
schema-valid example of what a correct conclusion over a recording of that
class looks like, independent of any specific recording. Each is checked in
a unit test (`committed_exemplars_parse_and_validate` in
`src/analysis/ground_truth.rs`): deserialize, then `Assessment::validate()`
must pass structurally (evidence grounding, non-empty summaries, etc.).

Once real corpus entries exist for a class, extend the check end-to-end:
extract features from a verified recording of that class and call
`exemplar.validate_against(&record)` — this additionally resolves every
evidence pointer against the real record and enforces the
acquisition-window confidence cap. That closes the loop the original
recording-assessment design asked for: "a hand-written example or two to
validate the schema end-to-end."
