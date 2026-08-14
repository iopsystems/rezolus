# `rezolus parquet convert` — raw recordings to parquet

Status: **Implemented 2026-08-13** in `src/parquet_tools/convert.rs` (38 unit
tests). Two things changed during implementation, both noted inline below:
`--metadata` rejects a malformed argument instead of dropping it the way
`record` does, and the output's permissions are reset after the staged temp
file is persisted.

## The problem

`rezolus record -f raw` writes concatenated msgpack snapshots. Raw is the right
choice for a long unattended capture: it costs the recorder nothing at write
time, and finalization is a byte copy rather than a schema-building conversion
that scales with run length. Agrippa's `launch_workboat.py` records this way for
exactly that reason — a multi-hour boat run must not risk losing the recording to
a SIGKILL during a slow parquet finalize.

The cost is that nothing can read the result. Raw→parquet conversion only exists
inside `record` and `hindsight` at finalize time, as an internal step. Once a
raw file is on disk — and in practice zstd-compressed by whatever ships it off
the boat — the viewer, the MCP tools and `parquet metadata` all reject it. The
only recovery today is a hand-written program against
`metriken_exposition::MsgpackToParquet`.

This adds the offline complement to `record -f raw`.

## Non-goals

**No `.rez` output.** A `.rez` archive holds one table per sampler at that
sampler's own cadence, and every observation carries acquisition-window columns
(`<m>:window_begin` / `<m>:window_width`) that the query engine consumes to put
uncertainty bounds on `rate()`. A raw stream is whole-agent snapshots on a
single recorder clock; the per-sampler cadence and the windows were never in it.
Synthesizing them would fabricate uncertainty bounds that look measured. If a
recording needs to be a `.rez`, it has to be recorded as one (`-f rez`).

**No change to `record`.** `record -f raw` stays exactly as it is. This is a
separate offline command, not a flag on the recorder.

**No streaming conversion.** See "Why compressed input needs a temp file".

## CLI surface

```
rezolus parquet convert <FILE> [-o OUTPUT] [--interval DUR]
                               [--systeminfo FILE] [--descriptions FILE]
                               [--metadata KEY=VALUE]... [--force]
```

`<FILE>` is a raw recording, plain or zstd-compressed.

**Compression is detected by magic bytes, not by extension.** A recording that
arrives as `rezolus.raw.zst`, one that is zstd-compressed but named `.raw`, and
a plain `.raw` all work. Extension-based detection would fail on exactly the
files that get moved and renamed between machines, which is every file this
command exists to handle.

**`-o` defaults to the input path** with `.zst` and `.raw` stripped and
`.parquet` appended: `rezolus.raw.zst` → `rezolus.parquet`. An existing output
is refused without `--force`, matching `combine` and `filter`.

**`--interval` overrides the stamped `sampling_interval_ms`.** The default is
*inferred* from the snapshot timestamps, not hardcoded to the recorder's 1s
default. A recording made at `--interval 250ms` would otherwise be silently
stamped 1s, and every rate in the viewer would be wrong by 4× with nothing on
screen to say so. The value used, and whether it was inferred or explicit, is
logged.

**`--systeminfo FILE` and `--descriptions FILE`** read JSON from a file, or from
stdin with `-`, mirroring `annotate --systeminfo`. Both are validated as JSON
before anything is written; `--descriptions` must additionally be an object of
string→string. These two keys cannot be recovered from a raw file — the recorder
fetches them from the agent's HTTP endpoints at record time — so the only way to
get them into a converted recording is for the operator to supply what they
saved alongside it.

**`--metadata KEY=VALUE`** is repeatable with the same semantics as `record
--metadata`. `source=rezolus` is stamped unless `--metadata source=…` overrides
it.

*Divergence from `record` (implementation):* `record` parses these with
`filter_map` and silently drops an argument it cannot split on `=`. `convert`
rejects it instead. Quietly producing a recording that is missing the label you
asked for is worse than refusing to start, and unlike a live recording there is
nothing lost by exiting — the input is still there to retry against.

## Structure

New module `src/parquet_tools/convert.rs`, registered in `mod.rs` the way the
other five subcommands are: a `mod convert;` declaration, a `.subcommand(...)`
block carrying `long_about` examples, a dispatch arm in the `args.subcommand()`
match, and a line in the parent `long_about`'s `SUBCOMMANDS:` list.

Four internal units, each testable without the others:

### `sniff(path) -> InputKind`

Reads the first bytes of the file and classifies: zstd (`28 B5 2F FD`), parquet
(`PAR1`), tar (`ustar` at offset 257), else raw msgpack. Pure classification.

Detecting parquet and tar matters for the error message, not for conversion —
someone will pass a `.parquet` or a `.rez` to this command, and "input is
already a parquet file" beats a msgpack parse error forty megabytes in.

### `materialize(path, kind, out_dir) -> impl Read + Seek`

A plain file passes through. A zstd file is decoded through `zstd::Decoder` into
a `tempfile::NamedTempFile` created in the **output's** directory, then reopened.

*Why compressed input needs a temp file:*
`MsgpackToParquet::convert_file_handle` takes `Read + Seek` and makes two passes
— one to build the schema across every snapshot, one to write rows — rewinding
between them. A zstd stream decoder is not seekable, so the decompressed bytes
have to exist somewhere. The temp file goes in the output directory rather than
`/tmp` because the expansion is large (13× on the observed sample: 16 MiB → 220
MiB) and `/tmp` is frequently a small tmpfs; putting it next to the output means
if there's room for the result there's room for the intermediate. `NamedTempFile`
cleans up on drop and on crash.

Adds `zstd = "0.13"` as a direct dependency. It is already in the build graph via
`parquet`'s `zstd` feature, so this costs no additional compile time.

### `infer_interval(reader) -> Option<Duration>`

Deserializes up to 11 snapshots, takes the **median** of consecutive
`Snapshot::systemtime()` deltas, rounds to the nearest millisecond, and rewinds.
Median rather than mean so that one stalled sample — a scheduling hiccup, a slow
agent scrape — doesn't drag the inferred interval off the real cadence.

Returns `None` for a file with fewer than two snapshots; the caller falls back to
1s and logs that it did.

### `build_converter(...)`

Assembles the `MsgpackToParquet` with `MAX_ROW_GROUP_SIZE` from
`parquet_metadata.rs` and the same file-level metadata keys
`recorder::build_parquet_converter` writes: `sampling_interval_ms`, `source`,
user `--metadata`, `systeminfo`, `descriptions`.

Deliberately *not* shared with the recorder's function. That one is wired to
`RecordingConfig` and `EndpointState`; reaching it from here would mean
constructing a fake endpoint state, which couples the offline path to the
recording path's types for no benefit. The metadata key list is short and both
sides are covered by tests that assert the exact keys.

## Error handling

Every failure exits non-zero with an `error: …` line on stderr, consistent with
the other `parquet` subcommands:

- input is parquet or tar → say which, and that conversion is not needed
- input is not a recognized recording → say so before the long read
- truncated or malformed snapshot mid-stream → report the byte offset
- output exists without `--force`
- `--systeminfo` / `--descriptions` not valid JSON (or descriptions not an
  object of string→string)
- decompression or write failure, including ENOSPC on the temp file

A partial output file is never left behind on failure: the parquet is written to
a temp file in the output directory and renamed into place only on success.

*Found in verification (implementation):* staging through `NamedTempFile` also
carried its 0600 mode onto the output, so a converted recording was private
where a recorded one is group-readable. The mode is now reset after `persist` to
whatever plain file creation yields under the current umask, taken from a
reference file created in the same directory (the process umask cannot be read
without temporarily setting it). Covered by
`output_permissions_match_a_normally_created_file`.

## Testing

Unit tests in the module, following the `annotate.rs` / `filter.rs` pattern of
building `Snapshot::V2(SnapshotV2{…})` values and tempfiles:

- `sniff` on all four magics, plus a zstd file with no `.zst` extension and a
  `.zst`-named plain file — extension must not decide the outcome
- output-path derivation: `x.raw.zst`, `x.raw`, extensionless, explicit `-o`
  wins, existing output refused without `--force`
- `infer_interval` at 1s and 250ms, `None` for a single snapshot, and a stream
  with one jittered gap to pin the median-not-mean behavior
- end-to-end: synthesize a raw stream with `to_msgpack`, convert both the plain
  and zstd'd forms, read the footer back, assert `sampling_interval_ms`,
  `source`, `systeminfo`, `descriptions`, and row count. This is the test that
  would have caught the seekability problem.
- malformed inputs: truncated mid-stream, and a parquet file as input — both
  exit non-zero with the specific message

## Increment: warning on an implausible inferred interval

Inference always produces a number, and until now it produced it silently. Two
cases make that number a poor description of the recording, and both are
invisible in the output file:

**The clamped case.** A median below 1ms cannot be represented by
`sampling_interval_ms` and is clamped up to 1ms, so every rate computed against
the file understates the real cadence.

**The irregular case.** When the sampled gaps do not cluster around any single
value — a recording stitched from two sources, or one full of restart gaps —
the median is arithmetic rather than meaningful, and no single stamped interval
describes the file.

`infer_interval` returns `Option<Inferred>`, pairing the interval with an
optional `IntervalConcern` (`Clamped` or `Irregular`). `Converted` carries the
concern out to `run`, which prints one `warning:` line to stderr. The conversion
still succeeds and exits 0: the stamped value is the best available reading, not
a failure. An explicit `--interval` never warns — the operator asserted it, and
`interval_millis` already refuses the value it could not represent.

### The irregularity rule

Count the sampled deltas within ±25% of the median; warn when fewer than 60%
qualify.

Those constants have one job: **not firing on the case the median exists to
absorb.** `infer_interval` uses a median precisely so a single stalled sample
does not move the answer, so a rule that then warns about that stall would
contradict the design it is reporting on. One stall in ten deltas leaves 90%
inside the band — silent. A recording stitched from a 1s half and a 250ms half
leaves about 50% — warned. The rule fires when there is no dominant cadence at
all, which is exactly when any single stamped value misleads.

### Remedies differ, so the two warnings read differently

The irregular warning names `--interval` as the fix. The clamped warning must
not: `interval_millis` rejects an explicit sub-millisecond value too, so there
is no value the operator could pass. It states the format limit instead —
`sampling_interval_ms` holds whole milliseconds, and this recording is faster
than that. Offering a remedy that the next command refuses would be worse than
offering none.

### Scope of the verdict

Inference reads only the first `INTERVAL_SAMPLE_SNAPSHOTS` snapshots, so the
concern describes the sample the interval was drawn from, not the whole file. A
recording that turns gappy later will not warn. This is deliberate rather than a
gap to close: the warning is evidence about exactly the data the stamped value
came from, and widening it to the whole file would mean a full read to produce
a number that is already only a header-derived estimate.

## Verification findings (implementation)

The `document-feature` loop (5 blind user-simulations + a fresh-eyes critic per
round, 3 rounds) passed 5/5 blind sims in every round. Two of its findings were
not documentation problems:

**`-i` collided across sibling subcommands.** `rezolus parquet metadata -i FILE`
means the *input file*; `convert -i 250ms` meant the *interval*. An agent
generalizing from one to the other got `expected number at 0` from the duration
parser, with the required positional also missing. Fixed in the CLI, not the
prose: `--interval` has no short alias, so `-i` now fails with `unexpected
argument '-i' found`.

**A sub-millisecond `--interval` stamped zero.** `sampling_interval_ms` holds
whole milliseconds and `Duration::as_millis` truncates, so `--interval 500us`
wrote `sampling_interval_ms=0` — a divide-by-nothing for every rate computed
against the file. The inference path had a `.max(1)` floor; the explicit path
never did. Now a single `interval_millis` rounds to the nearest millisecond and
rejects anything below 1ms outright, since rounding 500us up to 1ms would
understate every rate by half with nothing in the file to say so. Covered by
`sub_millisecond_interval_is_rejected` and
`sub_millisecond_precision_rounds_to_the_nearest_millisecond`.

## Deferred

`parquet annotate --descriptions` — a converted recording can get its
`systeminfo` back via `annotate --systeminfo`, but descriptions have no annotate
route, so the only recovery is a `--force` reconvert of the original raw input.
Deliberately out of scope here; tracked in `docs/backlog.md` under
"Parquet / recorder" alongside the pre-existing Prometheus-`# HELP` case.

## Documentation

Run the `document-feature` skill after implementation: it writes the `--help`
and README text and dispatches a fresh subagent that has never seen the code to
prove the help is usable on its own.

## Prior art

The one-off that motivated this converted a 20m 24s agrippa recording
(`rezolus.raw.zst`, 16 MiB → 220 MiB → 8.7 MiB parquet, 1189 columns × 1225
rows) in 7.6 seconds, using `MsgpackToParquet` with `sampling_interval_ms` and
`source` stamped by hand. The output reads correctly in `parquet metadata` and
answers `mcp query 'sum(rate(cpu_usage[1m]))'`. This design is that program plus
input detection, interval inference, metadata flags, and error handling.
