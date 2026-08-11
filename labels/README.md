# Label-Generation Harness

Runbook for `labels/` — the controlled-experiment harness that produces
recordings whose bottleneck is *known by construction*, so candidate
assessments (from a frontier model, and eventually a fine-tune) can be
verified against ground truth rather than vibes-checked. Full design:
`docs/superpowers/specs/2026-08-10-label-harness-design.md`.

This is sub-project #3 of the recording-assessment pipeline. Phases 1-3
delivered the record format (`OverviewRecord` v2), the extraction library,
and the `rezolus mcp extract-features` front door. This harness adds truth.

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
   target/release/rezolus mcp extract-features labels/corpus/<class>/<experiment-id>/recording.rez > record.json
   ```

2. Evaluate `GroundTruth::verify` against the extracted record. There is no
   `verify-ground-truth` front door in v1 (named Phase-5 item, not YAGNI'd
   away, just not built yet) — do this by loading both JSON documents in a
   small script, or a scratch test, that deserializes `GroundTruth` from
   `ground_truth.json` and `OverviewRecord` from `record.json` and calls
   `verify()`. As a library-level sanity check that the schema and the
   committed ground-truth documents themselves are well-formed, run:

   ```bash
   cargo test analysis::ground_truth
   ```

3. **Only verified pairs stay in the corpus.** If `verify()` returns
   `Err(errs)`, either the experiment didn't induce the expected bottleneck
   (investigate/retry) or the ground-truth document needs calibration (see
   below) — do not keep an unverified pair in `corpus/`. Record the outcome
   in the calibration log.

## Calibration log

| date | class | experiment id | verify outcome | notes |
|------|-------|----------------|-----------------|-------|
|      |       |                |                 |       |

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
