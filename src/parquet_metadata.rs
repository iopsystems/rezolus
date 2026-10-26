#![allow(dead_code)]

// ── Top-level parquet footer keys ───────────────────────────────────────

/// The keys a reader looks up (`source`, `version`, `service_queries`,
/// `events`, `per_source_metadata`, ...) are `dashboard::keys`, shared with
/// the viewers, `report-save` and the `recording` tools; the ones the
/// binary names through this module are re-exported here. What remains
/// below is what only rezolus writes.
pub use ::dashboard::keys::{
    KEY_DESCRIPTIONS, KEY_EVENTS, KEY_SERVICE_QUERIES, KEY_SYSTEMINFO, KEY_VERSION,
};

/// The producing agent's **counter epoch**: an opaque id that changes when,
/// and only when, every cumulative counter in the recording restarted from
/// zero together — which for a process-scoped producer is once per process.
///
/// Two recordings with equal epochs over overlapping time are two
/// observations of one monotonic series. A change means every counter reset
/// at that point, which a consumer cannot otherwise distinguish from a
/// counter that merely wrapped.
///
/// Name and semantics follow dendro's `keys::PRODUCER_EPOCH`, so an archive
/// written here reads the same to any consumer of that format.
///
/// **Source-wide, and it does not cover a single counter.** A counter that
/// wrapped, or that a sampler zeroes on read, did not restart the process, so
/// this says nothing about it; that needs a per-counter generation.
pub const KEY_PRODUCER_EPOCH: &str = "producer_epoch";

/// Every producer epoch a recording observed, in order: a JSON array of
/// `{"epoch": <id>, "from_ts": <first row timestamp>, "version": <agent
/// version>}`. Written when the agent restarts mid-recording; absent when no
/// restart was seen. The last entry is the current epoch, which is also under
/// [`KEY_PRODUCER_EPOCH`]. Every cumulative counter reset at each entry's
/// `from_ts` after the first.
///
/// Name, shape and the first two fields follow dendro's
/// `keys::PRODUCER_EPOCHS`; `version` is rezolus's addition, so a reader can
/// tell which agent build wrote the rows from each point on.
pub const KEY_PRODUCER_EPOCHS: &str = "producer_epochs";

/// JSON-serialised user selection/filter state saved by the viewer.
pub const KEY_SELECTION: &str = "selection";

// ── Keys nested under `per_source_metadata.<source>.<entry>` ─────────

/// The role this source plays in the recording. Known values:
/// - `"service"` — the system under test (e.g. an LLM inference server)
/// - `"loadgen"` — the load generator / benchmark tool (e.g. llm-perf)
pub const NESTED_ROLE: &str = "role";

/// Per-source nested key: JSON array of sampler status (from the agent's
/// `/samplers` endpoint). Present only for Rezolus-agent sources.
pub const NESTED_SAMPLER_STATUS: &str = "sampler_status";

// ── Report marker ───────────────────────────────────────────────────

/// Recording-level marker set by Save as Report when the report is
/// column-trimmed. Presence flips the viewer into report mode — empty
/// section list, frontend defaults to `/report`. String-typed so future
/// variants (`"full"`, etc.) don't need a schema bump.
pub const KEY_REPORT: &str = "report";

/// Canonical `KEY_REPORT` value written by the current writer.
pub const REPORT_VALUE_TRIMMED: &str = "trimmed";
