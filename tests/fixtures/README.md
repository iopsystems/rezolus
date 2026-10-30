# Cross-language test fixtures

Files here are **generated**, checked in, and asserted against by more than one
language. Do not hand-edit them.

## `stream-frames/wide.bin`, `stream-frames/long.bin`

Pin the bytes the agent's `/metrics/stream` sends for one fixed sequence of
passes, in the wide and long layouts, through metriken-exposition's
`StreamSubscription` with the agent's pass types and negotiation, as the
handler serves it (`src/agent/exposition/http/mod.rs`, tests in
`stream_frame_fixture`). The rows chunks are as the agent's own frame code
(`StreamState`, #1434) wrote them; the opening chunk's handshake also carries
`metriken.frame_interval` and `metriken.update_floor`. metriken's
`metriken-exposition/tests/stream_frames.rs` and
`metriken-storage/tests/stream_frames.rs` carry copies of the subscription
without negotiation, whose handshake has neither key.

Each file is one record per chunk the stream yields, the opening chunk
(preamble and handshake) first, then one per interval: a little-endian `u32`
byte length followed by the chunk's bytes. The producer has a fixed uuid,
clock anchor and labels.

The sequence: a first pass with two windowed groups and one windowless group;
the same pass again (an empty frame); a pass where one group's window did not
advance; no pass; a pass two intervals later, with one group gaining a member;
a wall clock step backwards (an empty frame with a clamped `seq`).

Regenerate with `REZOLUS_BLESS=1 cargo test stream_frame_fixture`, only when
the frames are meant to change.

## `display_wire_mixed.json`

Pins the display-mode binary layout (`crates/dashboard/src/display_wire.rs`,
`encode_display_binary`) that the viewer frontend decodes in
`src/viewer/assets/lib/charts/display.js` (`decodeDisplayBinary`).

**Why it exists.** The layout has two *optional* column groups, and the failure
mode is silent: a decoder that reads them in the wrong order, or reads one for a
series that never flagged it, does not error — it just returns plausible-looking
numbers, with every *later* series in the buffer shifted into the wrong bytes.

Before this fixture, both sides were tested — but each against its own
hand-written mirror of the other. The Rust test re-implemented a decoder; the JS
test re-implemented an encoder. Two mirrors can drift the same way and stay
green, and neither notices. This file is one buffer from the *real* encoder plus
the decode it should produce, so every implementation asserts against the same
bytes instead of against a copy of itself.

**Contents.**

| key | meaning |
|---|---|
| `hex` | the encoded body, hex-encoded (no dependency needed to read it in any language) |
| `expected` | the decode each series should yield — flags and every column |
| `budget` | the header's `budget` field |
| `_layout` | the column order, restated where a consumer will actually look |

`NaN` is written as `null` in `expected`: JSON has no NaN, and a non-finite band
edge means "no band at this point" — the same thing the raw f64 means.

**What the fixture deliberately contains**, each property catching a distinct
mistake:

- all four flag combinations (`unc` only, `interp` only, both, neither)
- **different point counts per series**, so a decoder assuming a uniform stride
  misaligns instead of coincidentally working
- the both-flags series **first**, so an off-by-one propagates into everything
  after it
- distinct values in every column, so a swapped column reads as wrong data
  rather than as plausible numbers
- a **partial** band within one series (some points `None`) — the shape a hole
  actually produces

**Regenerating.** A deliberate layout change should regenerate the fixture; the
resulting diff is the review artifact, showing exactly which bytes moved:

```bash
UPDATE_FIXTURES=1 cargo test -p dashboard the_shared_fixture
```

If a test fails here and you did *not* mean to change the wire, do not
regenerate — a decoder is about to start reading the wrong columns.

**Asserted by.**

- `crates/dashboard/src/display_wire.rs` → `the_shared_fixture_matches_the_encoder`
- `tests/display_wire_fixture.test.mjs`

**External consumers.** Anything implementing a third decoder against this wire
(e.g. a service consuming the display endpoint directly) can assert against this
file without reimplementing the encoder — which is the point, since a
reimplementation is exactly what drifts.

**Read it by URL; don't vendor a copy.** Fetching it from `main` means a
deliberate wire change breaks the external decoder's test at the same moment it
breaks ours — which is the entire value. A vendored copy goes stale silently,
and worse, it arrives *without this README*: the regeneration warning gets
separated from the bytes it guards, which is the same failure the warning is
about. That is why the warning is duplicated into the JSON's own `_comment`
field, so a raw fetch carries it too. If you genuinely must vendor, copy that
field along with the bytes.

## `legacy/simple_capture.parquet`

A standalone parquet recording from a 5.x recorder, formerly the static
site's sample. It is the input of `recording upgrade`'s tests and of the
refusal tests: every command but `upgrade` must refuse it and print
`rezolus recording upgrade FILE`.

## `upgraded/*.dendro`

`rezolus recording upgrade` of `legacy/simple_capture.parquet` and of the
`.rez` archives in `crates/rez/tests/fixtures/rez/` (`v2`, `v3`, `v3-multi`,
`v3-live`, `v3-wal-only`), for the WASM viewer's node tests and
`viewer_smoke.sh`, which open `.dendro` only. The Rust tests convert their
fixtures when they run. Regenerate one, only when the converter changes, with

    rezolus recording upgrade crates/rez/tests/fixtures/rez/v3.rez -o tests/fixtures/upgraded/v3.dendro

after removing the old file. Two conversions of one input are not
byte-identical, so no test pins these files' bytes.
