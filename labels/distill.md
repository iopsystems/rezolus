# Distillation Runbook

Runbook for distilling verified corpus pairs (`record.json` + `ground_truth.json`,
see `README.md`) into candidate `Assessment` documents that become the labeled
training corpus. This is sub-project #4 of the recording-assessment pipeline.
Design: `docs/superpowers/specs/2026-08-11-distillation-pipeline-design.md`.

The generation loop is agent-driven, like the label harness itself: no new
infra, just a documented procedure any agent (or later, an API-driven tool)
follows, and a real mechanical gate — `src/analysis/ground_truth.rs` — that
decides acceptance. An assessment only enters the corpus if it passes the
gate; the gate, not the generator's identity, is what makes a label
trustworthy (see "Honesty note" below).

## The prompt template

Fill in exactly two placeholders and paste the result to the model doing the
distillation:

- `{{RECORD_JSON}}` — the full contents of the corpus entry's `record.json`
  (the extracted `OverviewRecord`).
- `{{CROSS_CLASS_EXEMPLAR_JSON}}` — the full contents of
  `labels/exemplars/<other-class>.assessment.json` for a class **other than**
  the one this record belongs to. E.g. when distilling a `cpu_saturation`
  corpus entry, paste in `labels/exemplars/scheduler_contention.assessment.json`
  (and vice versa). **Never paste in the same-class exemplar** — that would
  leak the expected `subsystem`/`mechanism`/`direction` answer straight into
  the prompt. With exactly two v1 classes this means "the other one"; if a
  third class is ever added, pick any exemplar whose class differs from the
  record's.

The instruction text itself is class-agnostic and copy-pasteable verbatim —
it names no class, subsystem, mechanism, or direction, and does not hint at
what the record's answer should be. The ground truth document
(`ground_truth.json`) is deliberately **not** part of the prompt.

````text
You are assessing a single Rezolus performance recording. Below is a curated,
deterministic feature summary of that recording (the "record") — not raw
time series, but per-metric statistics, detected anomalies, regime shifts,
cross-metric correlations, resource-consumer rankings, and coverage
information extracted from it.

Your task: produce ONE JSON document, and nothing else, that is your
grounded conclusion about what (if anything) is limiting this workload. The
document must conform exactly to the Assessment schema described below.

## Output shape

Output valid JSON only — no prose, no markdown fences, no commentary before
or after. All enum values are bare strings (e.g. "High", not {"High": {}}).
The shape is:

```json
{
  "schema_version": 1,
  "overall": {
    "summary": "one-line claim about the whole recording",
    "confidence": "Low | Medium | High",
    "evidence": [
      {
        "metric": "metric name from the record, optional",
        "anomaly_index": 0,
        "regime_shift_index": 0,
        "correlation_index": 0,
        "rationale": "one-line tie from this record element to the claim"
      }
    ],
    "recommended_action": "concrete next step, optional",
    "status": "Actionable | Inconclusive | NoLimitingFactor",
    "data_quality": {
      "coverage_gaps": ["subsystem names absent from the recording"],
      "uncertainty_limited": false,
      "note": "optional free text"
    }
  },
  "findings": [
    {
      "summary": "one-line claim",
      "confidence": "Low | Medium | High",
      "evidence": [ "... same shape as above ..." ],
      "recommended_action": "optional",
      "priority": "High | Medium | Low",
      "kind": {
        "type": "Bottleneck",
        "subsystem": "e.g. cpu, scheduler, blockio, network, memory",
        "mechanism": "specific mechanism, e.g. cpu_saturation, runqueue_latency",
        "direction": "short phrase describing which way it's pinned"
      }
    },
    {
      "summary": "one-line claim",
      "confidence": "Low | Medium | High",
      "evidence": [ "..." ],
      "priority": "High | Medium | Low",
      "kind": {
        "type": "NeedsMetric",
        "missing": "name of the absent metric/subsystem, required, non-empty",
        "would_resolve": "what having it would let you conclude"
      }
    }
  ],
  "ruled_out": [
    {
      "summary": "hypothesis you actively dismissed",
      "confidence": "Low | Medium | High",
      "evidence": [ "the dismissing signal, same shape as above" ]
    }
  ]
}
```

`evidence[]` fields (`metric`, `anomaly_index`, `regime_shift_index`,
`correlation_index`) are each optional — include only the ones that apply to
that item — but `rationale` is always required. `recommended_action` is
optional everywhere and should be omitted (not null) on `ruled_out` items,
which describe dismissed hypotheses, not actions. `ruled_out` items carry no
`priority` or `kind` — they are plain claims.

## Authoring rules (mechanically enforced — a violation is rejected, not just discouraged)

1. **Every claim needs a non-empty `summary`.** Whitespace-only is rejected.
2. **`High` confidence requires evidence with a resolvable pointer.** A
   `High`-confidence claim (in `overall`, any `findings[]` item, or any
   `ruled_out[]` item) must cite at least one `evidence[]` item that names a
   real `metric` from the record, or a real `correlation_index` into the
   record's `correlations[]`. Evidence carrying only a `rationale`, or only
   an `anomaly_index`/`regime_shift_index` with no `metric` alongside it,
   does not count as a resolvable pointer and cannot ground `High`. Every
   evidence pointer you write must actually resolve: the `metric` name must
   appear in the record's `metrics[]`, and any `anomaly_index` /
   `regime_shift_index` must be a valid index into *that specific metric's*
   `anomalies[]` / `regime_shifts[]` array (not some other metric's).
   `correlation_index` must be a valid index into the record's top-level
   `correlations[]`. Do not invent metric names, indices, or numbers that
   are not actually in the record below.
3. **Acquisition-window uncertainty caps confidence.** Each metric in the
   record may carry an `uncertainty` block with `within_band: true/false`.
   If `within_band` is `true`, that metric's movement sits inside its
   measurement error band — it is not distinguishable from noise. A
   `High`-confidence claim may not rest on a metric whose `within_band` is
   `true`; if that is your only support, either find corroborating evidence
   elsewhere in the record or drop the claim to `Medium`/`Low`.
4. **`findings[]` items are prioritized and typed.** `priority` is
   `High`/`Medium`/`Low`. `kind` is exactly one of:
   - `Bottleneck { subsystem, mechanism, direction }` — a resource or
     subsystem actually limiting the workload, all three fields short and
     specific.
   - `NeedsMetric { missing, would_resolve }` — a genuine data gap
     preventing a conclusion; `missing` must be non-empty and name the
     specific absent metric or subsystem; `would_resolve` says what having
     it would settle. Use this instead of guessing when coverage is
     insufficient.
5. **`ruled_out` discipline.** Only list hypotheses you actively considered
   and dismissed, with the dismissing evidence — not a padded list of
   irrelevant subsystems. If you cite a metric as the dismissing signal, it
   must be one the record actually emits (rule 2's resolution requirement
   applies here too).
6. **`overall.status` is a whole-recording judgment**, not a finding:
   `Actionable` (you have a grounded, actionable conclusion),
   `Inconclusive` (the evidence doesn't support a confident claim either
   way), or `NoLimitingFactor` (the workload isn't bottlenecked by anything
   visible in this recording).
7. **`overall.data_quality` must be honest.** `coverage_gaps` should name
   subsystems the record itself reports absent
   (`context.coverage.subsystems_absent`) if they matter to your
   conclusion; `uncertainty_limited` should be `true` if any claim you made
   was constrained by an acquisition-window band per rule 3.
8. **Ground every claim in the record below.** Do not reference prior
   knowledge about what this experiment "should" show — reason only from
   the record JSON and your own analysis of it.

## Example (different class — form only, not the answer)

The document below is a complete, schema-valid `Assessment` for a DIFFERENT
recording than the one you are assessing, and a DIFFERENT bottleneck class
than whatever this record turns out to show. It demonstrates the *shape* of
a correct, well-grounded conclusion — how evidence is cited, how confidence
and uncertainty interact, how `ruled_out` is used — nothing more. Do not let
its subsystem, mechanism, direction, or wording bias your conclusion about
the record below; your answer must come entirely from that record's own
evidence, which will describe a different situation.

```json
{{CROSS_CLASS_EXEMPLAR_JSON}}
```

## The record to assess

```json
{{RECORD_JSON}}
```

Now produce the Assessment JSON for the record above. Output JSON only.
````

## The bounded repair loop

1. Run the candidate through the gate (below).
2. If it fails, re-prompt the same conversation with the gate's accumulated
   error list **verbatim** — do not summarize or reinterpret the errors,
   paste the exact `GATE FAIL` output — and ask for a corrected candidate
   that addresses every listed error.
3. Cap at **3 attempts total** (the original plus 2 repairs). If attempt 3
   still fails, log the rejection in the distill log (below) and stop; do
   not keep repairing past the cap.

**Caution — claim-equality error messages disclose the answer.** The gate's
claim-equality stage reports mismatches as `"{field} mismatch: candidate
{got:?} != truth {want:?}"` — the error text literally contains the correct
`subsystem`/`mechanism`/`direction` strings. A candidate can therefore be
repaired into passing the mechanical gate by simply copying those values
into the finding without the surrounding summary, evidence, or narrative
actually supporting the claim. **The distilling agent MUST review every
repair-loop output for narrative consistency** — read the repaired
`summary`, `evidence`, and `rationale` text and confirm they genuinely
support the (now gate-passing) categorical claim, not merely echo it. If a
repaired candidate only passes because the categorical fields were copied
from the error message rather than re-derived from the record's evidence,
**log it as rejected in the distill log even though the gate passed** — do
not store it in the corpus. This is a review judgment call, not something
the gate can check.

## The gate

The acceptance gate is `src/analysis/ground_truth.rs`'s
`gate_distillation_candidate` test: structural `Assessment::validate()`,
then cross-record `validate_against(record)`, then
`GroundTruth::matches_assessment(candidate)` (claim equality — exactly one
`findings[]` `Bottleneck`, `priority: High`, and `subsystem`/`mechanism`/
`direction` exactly equal to the ground truth's). The first two stages
short-circuit on failure (a structurally broken candidate can't be
cross-checked); the claim-equality stage accumulates all field mismatches.
`ruled_out` and any additional non-`Bottleneck` `findings[]` items are
**not** machine-gated — see the agent-review checklist below.

Run it against a candidate:

```bash
CANDIDATE=<path> CORPUS_RECORD=<path> CORPUS_TRUTH=<path> cargo test gate_distillation_candidate -- --ignored --nocapture
```

Where `CANDIDATE` is the candidate `assessment.json` you just authored,
`CORPUS_RECORD` is the corpus entry's `record.json`, and `CORPUS_TRUTH` is
the corpus entry's `ground_truth.json`.

## Storage layout

An accepted candidate is stored alongside the corpus entry it was distilled
against:

```
labels/corpus/<class>/<experiment-id>/assessment.json    # the accepted candidate
labels/corpus/<class>/<experiment-id>/provenance.json    # how it was produced
```

`provenance.json` schema:

```json
{
  "generator": "<model or agent identity>",
  "attempts": <n>,
  "date": "YYYY-MM-DD",
  "gate": "validators+claim-equality-v1"
}
```

`generator` identifies who/what authored the accepted candidate (e.g. a
model name or agent identity — whatever ran the prompt template and repair
loop). `attempts` is the 1-based count of gate runs it took to reach
acceptance (1 if it passed on the first try). `gate` is a fixed string
identifying which gate version accepted it, so a future gate change can be
distinguished from a re-distillation.

## Distill log

Every distillation attempt — accepted or rejected — gets a row:

| date | class | experiment id | generator | attempts | outcome | notes |
|------|-------|----------------|-----------|----------|---------|-------|
| 2026-08-11 | cpu_saturation | 019fef20-8fd1… | claude-sonnet-5 (blind agent) | 2 | ACCEPTED | Blind attempt 1: correct subsystem+mechanism, all validators passed; only the free-text direction mismatched. Repair conformed direction and removed a contradicting NeedsMetric hedge; review judged the final claim evidence-supported (system-wide cpu_usage shifts; cpu_cycles~cpu_usage r=0.99999), not gate-gamed. |
| 2026-08-11 | scheduler_contention | 019fef20-c9e8… | claude-sonnet-5 (blind agent) | 2 | ACCEPTED | Blind attempt 1: correct subsystem+mechanism; direction-only mismatch again. Single-field repair; narrative already described the mechanism. Calibration observation: the blind read shows queuing behind ~8/32 busy CPUs (cpu_usage far below machine ceiling) — the induced 4×nproc oversubscription likely ran inside a job-cgroup core subset; the truth's direction phrase is still accurate but the record tells a sharper story. |

Findings from the first distillation run (2026-08-11):
- **The `direction` field is the gate's friction point**: both blind
  generations diagnosed subsystem+mechanism correctly and failed claim
  equality ONLY on the open-vocabulary direction phrase. Named refinement
  for the next iteration: a constrained per-class direction vocabulary, or
  dropping `direction` from the equality gate (keeping it agent-reviewed).
- **The disclosure→conform loop worked as designed but demands the
  narrative review**: one repair legitimately removed a hedge to conform;
  the reviewing agent must (and did) check the remaining claim stands on
  the record's own evidence.

`outcome` is `ACCEPTED`, `REJECTED (gate)` for a candidate that never passed
within 3 attempts, or `REJECTED (narrative)` for a candidate that passed the
mechanical gate but was rejected on repair-loop review per the caution
above. `notes` records anything worth remembering: prompt-template
weaknesses discovered, which cross-class exemplar was used, why a candidate
was rejected, etc.

## Agent-review checklist (not machine-gated)

The gate only checks the `Bottleneck` claim's categorical fields and
structural validity. Before storing an accepted candidate, the distilling
agent must also confirm, by reading the candidate itself:

- **`overall.status == Actionable`** for a bottleneck class corpus entry
  (all v1 classes are bottleneck classes) — an accepted candidate that
  claims `Inconclusive` or `NoLimitingFactor` while still carrying a
  gate-passing `Bottleneck` finding is internally contradictory.
- **`overall`'s narrative agrees with the `findings[]` `Bottleneck`.** The
  `overall.summary`/`evidence`/`recommended_action` should describe the same
  bottleneck the `Bottleneck` finding names, not a different or vaguer
  story bolted on top.
- **`ruled_out` dismissals are plausible and cite emitted metrics.** Each
  `ruled_out` entry should be a hypothesis actually worth considering for
  this record, dismissed with a real signal from the record — not a
  generic list, and not evidence pointing at a metric the record doesn't
  emit.
- **No same-class exemplar was used in the prompt.** Confirm the exemplar
  pasted into `{{CROSS_CLASS_EXEMPLAR_JSON}}` was for a class other than
  this corpus entry's — re-check this explicitly before storing, since it's
  exactly the kind of mistake that silently leaks the answer.

## Honesty note

In agent-driven mode, the distilling agent authors the candidate — there is
no separate "trusted" generator distinct from an ordinary agent following
this runbook. That is fine: what makes an accepted label trustworthy is not
who or what produced it, but that it passed the mechanical gate (validators
+ claim equality) and the agent-review checklist above, both applied
uniformly regardless of generator identity. This same runbook — prompt
template, repair loop, gate, storage layout — is designed to drive a future
API-based generator with no changes beyond swapping who reads the prompt.
