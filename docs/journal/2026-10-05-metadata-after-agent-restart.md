# Recording metadata after an agent restart

**Status: done.** An archive written by `record` or `hindsight` records an
agent restart in its metadata.

## Problem

`record` and `hindsight` read the agent's `version`, `producer_epoch`,
`systeminfo` and metric `descriptions` once, when the recording opens, and
write them into its metadata. When the agent restarts mid-recording (a package
upgrade, a crash and restart), both noticed: the stream reconnects with a new
epoch in its handshake, and `record`'s scrape path reads the epoch off each
snapshot. Both logged a warning naming the two epochs and changed nothing else.
The rows after the restart were then described by the old process: its epoch,
so nothing downstream could tell every counter reset there; its version, so an
upgrade was invisible; and its descriptions, so a metric the new build added
had none. `docs/parquet_metadata.md` listed the epoch half as a known
limitation and named dendro's `producer_epochs` key as the fix. `version`,
`systeminfo` and `descriptions` were not listed.

## What is written

`src/recorder/restart.rs` builds the patch from the recording's current
metadata, the restarts seen and what the new process reports:

- `producer_epoch` becomes the newest epoch.
- `producer_epochs` is dendro's `keys::PRODUCER_EPOCHS`: a JSON array of
  `{"epoch", "from_ts"}`, one per process, `from_ts` being the first row from
  that process. Each entry also carries the `version` that process reported,
  which is rezolus's addition. The first restart seeds the array with the
  epoch the recording opened with, from its first row; absent means one
  process was seen.
- `version` and `systeminfo` become the new process's when it reports them.
- `descriptions` becomes the union of old and new, the new text winning for a
  metric in both, so rows from before the restart keep their descriptions.
- A key the user set with `record --metadata` (in practice `version`) is kept.

Decided: one `version` for the archive (the newest) plus a version per epoch in
`producer_epochs`, rather than a separate version history key. A reader that
needs the version at a given time reads it off the epoch entry whose `from_ts`
precedes it.

## When it is written

A restart is noted when the epoch changes (`note_epoch` in
`src/recorder/mod.rs`; the `Connected` arm in `src/hindsight/mod.rs`). Its
`from_ts` is the first row staged afterwards. Once every pending restart has a
`from_ts`, the new process's metadata is fetched (`fetch_agent_metadata`,
bounded by the scrape timeout) and the patch applied: `record` does this after
draining each tick's stream events and once more at shutdown
(`apply_restarts`), hindsight after the interval that carried the first row
(`record_restarts`). Two restarts between fetches each get an entry; the one
that came and went has no `version`, since only the newest process can be
asked.

The two archive formats take the patch differently, through the same helper
on each side (`RezStream::patch_metadata`, `HindsightBuffer::record_restarts`):
dendro's `update_metadata` merges a patch key by key, so the patch is sent; the
`.rez` writer replaces the whole map, so the merged map is sent. `RezStream`'s
event path (`merge_events`) now goes through the same helper.

Hindsight's buffer now opens with the epoch from the stream handshake rather
than from `/status`, falling back to `/status` when the handshake carries none:
the handshake names the process whose rows arrive, and the restart comparison
already used it.

## Not covered

- A parquet recording writes its metadata once, when it closes, with no
  history: `producer_epoch` is the last seen, the rest are from when it
  opened. A raw recording carries no metadata. For both, the warning is the
  record of the restart.
- Nothing reads `producer_epochs` yet. The viewer and `mcp describe-recording`
  could show restart points; that is follow-up work.
- 5.x is unchanged: its `.rez` metadata is per recording and 5.25.1 logs a
  warning on an epoch change.

## Verification

- Unit tests in `src/recorder/restart.rs`: the first restart seeds the
  history; a later restart appends; two restarts before one fetch; the
  descriptions union; a user-set version kept; an agent that reports nothing;
  a recording opened without an epoch; the key equals dendro's.
- `record_lifecycle::an_agent_restart_is_written_into_the_recording_metadata`:
  a stand-in agent closes its stream after five frames and comes back under a
  new epoch with a new version on `/`; the `.dendro` source's metadata names
  the new epoch and version, and `producer_epochs` has both, in order. Red with
  `apply_restarts` disabled.
- `hindsight_dump::an_agent_restart_is_written_into_a_dendro_buffers_metadata`
  and `..._a_rez_buffers_metadata`: the same for a hindsight buffer of each
  format, read from a dump; the `.rez` test also checks a startup key
  survives the whole-map replacement. Red with the metadata write disabled.
