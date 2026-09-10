# Cross-language test fixtures

Files here are **generated**, checked in, and asserted against by more than one
language. Do not hand-edit them.

## `display_wire_mixed.json`

Pins the display-mode binary layout (`crates/dashboard/src/display_wire.rs`,
`encode_display_binary`) that the viewer frontend decodes in
`src/viewer/assets/lib/data.js` (`decodeDisplayBinary`).

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
