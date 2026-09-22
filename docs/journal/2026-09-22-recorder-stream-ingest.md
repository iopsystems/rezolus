# The recorder consumes the replication stream

- **Opened:** 2026-09-22
- **Status:** **SHIPPED, measured** (iopsystems/rezolus#1272, `49cecd43`:
  `record --stream`, `stream::pump`, `Applied::for_writer`,
  `AgentRow::from_payload`, `RezStream::stage_stream`). Opt-in; scraping stays
  the default.
- **Result:** the streaming recorder uses **0.52x the CPU** of the scraping
  one for the same window, and the archive it writes holds the same segment
  bytes plus the identity index. The archive does not shrink, and under churn it
  grows — by design at this stage, see "What this does not do".
- **Driver:** #1224 Phase 3. #1261 built the subscriber and nothing called it;
  #1267 made a tick's index entries commit with its rows; #1268–#1271 put the
  producer's stamp on every row. This connects them.
- **Owner:** Brian Martin

## The impedance, and the decision

`StreamSubscriber::apply` hands back dendro `WalRow`s: `stream`, `ts`,
`wall_offset`, and an opaque payload. `StreamRecorderV3::stage_rows` wants
`AgentRows`, whose envelope carries `window`, `schema_hash`, `schema`, `arity`
and `approx_bytes` in the clear — the fields every staging decision reads. The
`rez::wire` module argues at length that a pipe which decodes every row to route
it is not a pipe.

Three ways to close the gap were on the table:

1. **A third staging path**, `stage_wal_rows`, keyed on dendro's shape. Rejected:
   `stage_rows` already exists because `ingest_v3` did, and the row-wire entry
   documents the cost of two copies of one decision list. A third is the same
   mistake once more.
2. **Change the producer** so the frame carries the envelope. Not available:
   `dendro::archive::WalRow` is dendro's type, and the point of #1261 was to use
   dendro's frames unchanged.
3. **Decode each payload once on the consumer** and rebuild the envelope. A
   stream payload *is* a `WalGroupRow`, and every field the envelope wants is in
   it: `window` and `schema_hash` verbatim, `arity` from the three value
   vectors, `approx_bytes` recomputed with the snapshot path's formula
   (`wal_group_row_approx_bytes`, pinned equal to `group_approx_bytes` on a
   group with absent slots and a histogram). A schema the producer put in the
   payload — its first mention of that generation on a connection — is lifted
   into the envelope and the payload re-encoded without it, so the envelope's
   contract (`row` never carries a schema; the consumer anchors) holds.

Option 3 shipped. The decode is the cost of reusing the staging decisions
verbatim, and it was measured rather than argued about: see below. The
`wire` module's objection stands for the *row endpoint*, where the point was
that a scrape body could be routed without a decode; on the stream the producer
has already filtered by window advance, so a decode per row is a decode per
group that actually read this interval.

Consumer-side window dedup is kept even though the producer dedups too: a
reconnect starts a fresh subscription whose `last_window` is empty, so the first
frame after a drop resends every group's current window, and only the recorder
knows which of those it already holds.

## Shape of the loop

A `pump` task per endpoint holds the `Subscription` and sends each applied
interval down one bounded channel (`STREAM_QUEUE_PER_ENDPOINT` = 16 per
endpoint). The tick loop drains it with `try_recv`, stages rows through
`stage_rows`, and puts the interval's index entries in the same `TickBatch`, so
they commit in the transaction that commits the rows they describe. One commit
per tick, as with scraping. The loop never awaits a connection: a loop that did
would stall every endpoint's commit on the slowest socket.

The recording is anchored on the handshake's `clock_anchor_wall_ns` and its
uuid is the epoch (`adopt_source`). `/status` is read first and the handshake
after it, so the handshake is the newer observation and wins: the rows are on
its timeline, and an agent that restarted between the two would otherwise be
anchored on a clock it no longer keeps. `Subscription::connect` now waits for
the handshake so `source()` is known before the recording is opened.

Failure classes are split by whether retrying can change the answer
(`stream::ConnectError`). `Unsupported` — a 404, the 409 a V2 agent gives, a
wrong content type, an endpoint declared `protocol=prometheus` — exits 1 at
startup and fails the recording if it appears mid-run. `Unreachable` is the
Pending-and-retry-each-tick case scraping has always had. A dropped stream is
reconnected by the pump after one interval (floored at 1 s) and the new
handshake's source is reported, so a restarted agent gets the same warning the
scrape path gives on a changed snapshot epoch.

The end of a run waits one bounded interval for the frame in flight. The agent
frames on its own boundary and the loop's deadline sits on the same boundary,
so the final frame lands milliseconds after the tick that ended the loop and
was never committed without the wait.

## Measured

`delta`, 32 cores, 120 s at `--interval 1s`, this build's agent with all
samplers, run concurrently. Recorder CPU is `user+sys` from `/usr/bin/time`;
agent CPU from `/proc/<pid>/stat`.

**One agent per recorder** (the honest comparison — see the next section for
why):

| | scrape | stream |
|---|---|---|
| recorder CPU | 1.47 s | 0.76 s |
| agent CPU serving it | 0.55 s | 0.78 s |
| archive | 4.93 MB | 5.94 MB |
| `caller_rows` entries | 0 | 6363 (411 KB of blobs) |
| rows per group table (max) | 78 | 75 |
| last row | 314.0 s | 315.0 s |

The recorder saves 0.71 s; the agent spends 0.23 s more building the index and
framing (that is the demand-gated identity #1266 measured, now with a demander).
Net across both processes, 1.54 s against 2.02 s. The size gap is entirely the
index the scrape path never records: on the shared-agent run below, segment
bytes were 4,220,831 against 4,193,458.

Manifests agree on anchor, epoch and labels. `sum(rate(cpu_usage[30s]))`
agrees to within the row-count difference. The ~1.5 s per row on **both**
transports is the 1 s interval beating against the 1 s TTL: a request landing
just after a boundary finds the cache a hair under a second old. Agent
behaviour, unchanged by this work.

## A measurement artifact worth recording

The first runs shared **one** agent between the two recorders, and the streamed
archive came out a strict subset of the scraped one: 81 rows against 87, the
missing stamps at 25.038, 45.038, 77.038, 103.038, 117.038 and 120.041 s. No
gap or skip warning on the stream. The mechanism: the scraper's request landing
after the stream's boundary read triggered a fresh pass; if the stream's next
read came late enough to trigger another, the scraper's pass was superseded
before the stream ever read it. The stream sends the *latest* pass per
interval, so a pass that two reads never straddle is never sent.

That is not a recorder defect and it is not reachable with the stream as the
agent's only consumer, where every pass is triggered by the stream's own read.
It is worth knowing for anyone running an exporter and a `--stream` recorder
against one agent: the recorder will see one pass per interval, which is what
its interval asked for. It cost one wasted iteration here: the end-of-run wait
was first justified by the 93-against-95 count from a shared-agent run, and
the count did not move when the wait went in. The wait is still right — the
last-row timestamp shows it — but the number that motivated it was this.

## What an adversarial review found

Two confirmed defects in the first cut, both in the same shape: the transport
bounded nothing the scrape path bounds.

- **A silent connection was never detected.** `next_interval` awaited the next
  chunk unbounded, and the reconnect `connect` was unbounded too. A peer that
  vanishes without a RST — a firewall dropping idle state, a proxy that stops
  forwarding — never closes the socket, so the pump sat in the read for the
  rest of the run, the endpoint stayed Active, and the recording finalized
  clean and short with no warning. The protocol already had the fix: the agent
  sends a frame every interval, empty when nothing is new, as the keepalive.
  The pump now bounds every socket wait by the scrape timeout and reports
  silence past it as `Dropped`. Proven with a route that sends the handshake
  and then hangs; the test fails with the bound removed.
- **Every non-2xx was `Unsupported`, so a transient 502 was fatal.** At
  startup that exited 1; on reconnect it ended the recording — and a proxy
  answering 502 while the agent behind it restarts is exactly when a stream
  has just dropped. 5xx, 408 and 429 are `Unreachable` now; 404, 409 and a
  wrong content type stay `Unsupported`. The original test covered only 404
  and 409, which is how it got through.

Three smaller ones, also fixed: the end-of-run grace was a `recv` with a
timeout, which returns at once on anything already queued, so under ctrl-c the
frame in flight was still dropped (now an unconditional sleep, then drain);
`for_writer` grouped by `(ts, wall_offset)`, so a relay stamping two passes at
one `ts` could collide on the WAL key (now by `ts` alone); and `open_stream`
read the handshake before `/status`, so an agent restarting between the two got
a restart warning pointing backwards (now `/status` first, so the handshake is
the newer observation). A wall-clock note on the reconnect cadence: it is
`interval` floored at one second, and the docs said "each interval".

What the review checked and found sound is listed in #1272; the runtime shape,
the channel, the WAL key from the real producer, the index entry keying, the
payload re-encode and the config guards all held.

## What this does not do

The archive does not get smaller, and under task or cgroup churn it gets
larger. Schemas still carry slot identity on main, so a slot changing hands
changes the schema hash and both transports re-anchor the whole group schema
in the next WAL row — and the stream then stores the index entry for the same
change on top. Identity is held twice. The saving the index was built for
arrives when it becomes authoritative and identity leaves column metadata,
which #1224 puts on the short-lived 6.0 cutover branch.

The reason that cannot be done from the stream alone is the reader, not the
writer. A group column's key today is descriptor plus identity — the table
builder gives a relabelled slot a new column — and every reader labels a column
from the metadata stored with it. `caller_rows` is written opaque and nothing
reads it. With identity gone from columns, one slot column spans several
occupants and attribution becomes a time-ranged join against the index. No
reader does that join; a 5.x archive written that way would open everywhere
with columns nothing can tell apart.

## Next

Readers before writers. A `--stream` archive from this build carries the index
**and** identity in the columns at the same time, which is the oracle the
reader work needs: a reader that joins columns to `caller_rows` must reproduce,
label for label, what the columns already say, on every table of a real
recording. That check disappears once the writer drops identity. So:

1. Teach `RezReader` (and through it both viewers, MCP, `parquet filter
   --metrics`) to resolve identity from `caller_rows`, verified against column
   labels on today's archives. The `crates/rez` read path has to do it without
   `metriken`; the `IndexEntry` encoding stops being opaque on the read side
   and needs pinning.
2. Then the cutover branch is small: the writer stops carrying identity in
   schemas, the agent stops resending schemas on slot changes, the format
   version bumps.

The wire half — identity-free schemas on the stream, merged back from the
subscriber's `SourceIndex` at anchor time — could be banked on main without a
reader change, at the cost of a second per-tick encode on the agent or a change
to `/metrics/rows` too. Not done here; the plan puts it on the cutover branch.

## Related

- #1224 (6.0 plan), #1261 (subscriber), #1266 (demand-gated identity), #1267
  (index entries commit with rows), #1268–#1271 (producer stamps), #1272 (this
  work).
- [The row endpoint's value is schemas, not encodes](2026-09-16-row-endpoint-schema-resend.md)
  — the transport this reuses and the measurement it inherits.
