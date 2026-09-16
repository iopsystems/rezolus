# The row endpoint's value is schemas, not encodes

- **Opened:** 2026-09-16
- **Status:** **OPEN — producer side in review** (iopsystems/rezolus#1237:
  `rez::wire`, `StreamRecorderV3::stage_rows`, `GET /metrics/rows`). The
  recorder that consumes it is not built yet. The premise correction below is
  the load-bearing result and is measured.
- **Result:** **2.6x smaller bodies**, measured end to end over 60 consecutive
  scrapes on real hardware — from a schema policy change, not from the encode
  saving the plan proposed (which measured net-negative).
- **Driver:** #1224's 6.0 plan lists a row-format endpoint as a main-branch
  item, motivated by transport cost: *"an endpoint serving already-encoded WAL
  rows makes the recorder a pipe, one encode total, performed on the agent."*
- **Owner:** Brian Martin

## The premise, and why it is wrong

The plan's pipeline argument is sound as far as it goes — recording a local
agent really does pay two msgpack encodes and a decode for data that started in
shared memory. What it does not establish is that those encodes are where the
cost *is*.

They are not. Measured with both transports driven over identical ticks — 944 metrics across
26 groups, 200 ticks, release build. Reproduce with
`cargo test -p rez --release row_transport_cost -- --ignored --nocapture`:

| transport | agent | recorder | body |
|---|---|---|---|
| snapshot | 49 µs/tick | 509 µs/tick | 89 KiB |
| rows | 121 µs/tick | 297 µs/tick | 92 KiB |

**1.71x on the recorder, 1.33x on total CPU.** Real, then — but see what the
schema policy is worth below, and note two caveats that cut it further: part of
the recorder-side gap is that the row decode is a plain `from_slice` while the
snapshot path uses the hardened `Snapshot::from_msgpack`, and under a *no-resend*
policy the transport alone goes negative on total CPU (0.90x), because once
schemas are gone the per-row framing is all that is left. The transport is a
second-order effect either way.

**An earlier draft of this entry recorded 0.99x — net negative — for the first
row.** That was measured with `rmp_serde::from_slice` on the snapshot path,
which is not what the recorder calls; `from_msgpack` adds depth-cap and
trailing-byte hardening over an untagged enum and costs materially more. The
bench now uses the same function the recorder does.

## What the same experiment did surface

Holding the transport fixed and varying only whether the producer resends
schemas:

| schema policy | agent | recorder | body |
|---|---|---|---|
| every tick (what the agent does) | 49 µs/tick | 509 µs/tick | 89 KiB |
| only on change | 5 µs/tick | 29 µs/tick | 4 KiB |

**17.6x on the recorder, 22x on the wire** — from a producer-side policy, with
the transport held fixed. An order of magnitude against the transport's 1.3x.

Confirmed on real hardware — `delta`, 25 healthy samplers, **45 acquisition
groups, 3,560 declared members**:

| | bytes |
|---|---|
| `/metrics/json` scrape | 655,621 |
| the same scrape, schemas removed | 80,079 |
| **schema share** | **87.8% — 8.2x smaller without** |

`/metrics/binary` for that scrape is 455,080 B.

`src/agent/exposition/http/snapshot.rs` emits `schema: Some(schema)`
unconditionally. `SkeletonCache` avoids *rebuilding* an unchanged schema, but
the schema still goes on the wire every tick, is re-encoded every tick, and is
re-decoded by every consumer every tick — 3,560 names and metadata `BTreeMap`s,
for values that did not change.

The machinery to avoid this already exists and has never had a producer:
`WalGroupRow.schema` is optional, `schema_hash` names the generation, and
`StreamRecorderV3`'s ring resolves a reference. `ingest_v3`'s own comment says
the `schema: None` path "is exercised only by malformed input in practice".

## What the endpoint actually delivers — and why it is not 8.2x

The 87.8% above is an **upper bound**: what a body would be with schemas
removed outright. The endpoint does not reach it. Measured end to end on
`delta` — agent up 30 s, then 120 consecutive scrapes of `/metrics/rows` and
`/metrics/binary` alternating at 1 s, medians over the last 60:

| | bytes/scrape |
|---|---|
| `/metrics/binary` | 559,458 |
| `/metrics/rows` | 212,308 |
| **ratio** | **2.64x smaller** (2.55x by total bytes; 58 of 60 scrapes under half) |

**2.6x, not 8.2x.** Two things account for the gap, and both are worth knowing:

**Residual schema churn.** A DEFAULT (non-declared) group's membership is
*value-derived* — the transitional V2-style sentinel skip, where index `idx` is
a member only if its current value is non-zero
(`src/agent/exposition/http/snapshot.rs`). So a counter's first non-zero tick
changes its group's membership, and therefore its schema hash, and therefore
forces a resend. For cumulative counters that is monotonic and settles, which is
visible in the run: rows bodies climb 170 KB → 212 KB while snapshot bodies
climb 492 KB → 559 KB as membership fills in. But it never fully stops — the
last-60 max is 423,323 B against a 212,308 B median, i.e. occasional near-full
bodies. Declared groups have no such churn: their membership is
registration-derived. **Migrating groups to declared member sets is a direct,
cheap multiplier on this endpoint**, and is the same churn #1224's item 2
(column identity into a secondary index) exists to kill.

**The row wire's own framing.** Each row is a separately-encoded payload carried
as an opaque `Vec<u8>`, plus its cleartext header. Holding the schema policy
fixed, that makes a rows body **1.6x larger** than the equivalent snapshot body
(7 KiB vs 4 KiB in the reproducible bench). So the floor is not the 80,079 B a
schema-less snapshot would be — it is roughly 1.6x that. Inlining the payload
rather than nesting it, or hoisting repeated stream names, would lower the floor;
not attempted here.

Both are follow-ups, not blockers: 2.6x on the wire and a recorder that no
longer decodes 3,560 names and metadata maps per tick is worth having on its
own.

## Why this cannot be fixed in `/metrics/binary`

That body is contractually self-contained, and load-bearingly so:

- `record --format raw` buffers scrape bodies verbatim
  (`src/recorder/mod.rs`, `Format::Raw`) and `recording convert` decodes them
  offline, much later, with no access to whatever earlier scrape carried the
  schema.
- The exporter and the live viewer each decode one body in isolation.

A recorder is the one consumer that keeps state across scrapes. So the fix is a
second endpoint for that consumer, not a change to the shared one — which is
the same conclusion #1224 reached about *where* the row endpoint belongs,
arrived at from the cost side rather than the transport side.

## Decisions

- **Build the row endpoint, for the schema reason, not the encode reason.** The
  encode saving is real but small; the schema saving is the order of magnitude.
  #1237's module docs state this explicitly so the next reader does not
  re-derive the wrong motivation from #1224.
- **The producer never anchors.** A `WalGroupRow`'s `schema` is `Some` only on
  the row re-anchoring a group for the segment accumulating in the *consumer's*
  archive, and two recorders scraping one agent rotate independently. Payloads
  carry `schema: None`; the consumer re-encodes the rare anchoring row — once
  per group per segment against a 300 s default `max_age`.
- **`?schemas=all` is mandatory, not a convenience.** `emitted_schemas` tracks
  what the agent has sent, not what a consumer holds. A recorder connecting
  mid-life, or one whose response was dropped in flight, recovers by asking for
  a full body. This keeps the agent free of per-consumer session state at the
  cost of one full body on connect.
- **Route on cleartext, never on the payload.** `stream`, `window`,
  `schema_hash`, `arity` and `approx_bytes` travel beside the opaque payload. A
  pipe that decoded each row to route it would not be a pipe; `arity` and
  `approx_bytes` additionally make a row recording segment at the *same*
  boundaries as a snapshot recording rather than merely hold the same values.

## What keeps the two transports honest

Structural, not just tested: `wal::wal_group_row` builds both paths' payloads
and `StreamRecorderV3::resolve_schema` is one schema ring for both.
`a_row_recording_and_a_snapshot_recording_agree` then drives two recorders in
lockstep through dedup, a schema change and a segment rotation and compares
staged WAL rows **as bytes**. Mutating the anchor rule, the dedup, or the window
each make it fail — checked, not assumed.

## Open

- **Declared member sets for default groups.** The single biggest multiplier on
  this endpoint, per the churn analysis above, and it helps segment compaction
  independently of the wire.
- **The row wire's framing overhead** (1.6x at equal schema policy). Worth an
  issue: inline the payload instead of nesting it, and hoist repeated stream
  names.
- The recorder side: negotiation, the unknown-hash retry, and falling back to
  `/metrics/binary` against an agent too old to serve rows.
- Whether the exporter and live viewer should grow a schema cache too. They
  *could* — the reason they do not today is the self-contained contract, which
  only `record --format raw` genuinely requires.

## Related

- #1224 (6.0 plan), #1237 (this work).
