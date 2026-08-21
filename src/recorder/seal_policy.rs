//! Segment sealing policy, shared by every `.rez` writer.
//!
//! This outlived the tar writer it was born in. `SealPolicy` answers "is this
//! open segment due?" and `SegmentAccount` maintains the byte and row counts
//! that question is asked against — both are properties of *segmenting a
//! recording*, not of the container it lands in, so the v3 SQLite writer and
//! hindsight's rolling buffer use exactly the same ones.

use std::time::{Duration, Instant};

/// Granularity of the first-seal stagger. A sampler's first segment closes at
/// `max_rows - (max_rows / (2 * STAGGER_BUCKETS)) * bucket` for a `bucket` in
/// `[0, STAGGER_BUCKETS)`, i.e. somewhere in `[max_rows / 2, max_rows]`. 64
/// buckets is ample spread for a dozen tables, and capping the reduction at
/// 50% bounds the startup cost to one short segment per sampler.
pub(crate) const STAGGER_BUCKETS: u64 = 64;

/// When an open segment is due to be sealed. Byte-first: the byte cap is the
/// one that bounds both the builder's memory footprint and the encoder's input,
/// and it is maintained O(1) per entry by `TableBuilder::push_row`.
///
/// **The age bound exists for the kill-loss window, not finalize cost.** The
/// byte and row caps alone bound finalize time and memory — a slow sampler's
/// open segment is naturally tiny — so age sealing only bounds how much data an
/// unclean kill loses. It is also what drives segment count, so the trade (loss
/// window vs segments and read-time merge width) is deliberate.
pub(crate) struct SealPolicy {
    pub max_bytes: usize,
    pub max_rows: usize,
    pub max_age: Duration,
}

/// The two caps and the age bound.
///
/// **Seal policy is not a CPU knob.** Sealing is a minority of what the
/// recorder burns — the per-tick scrape/decode/ingest path dominates — so
/// moving these caps trades finalize latency, peak memory and the kill-loss
/// window against each other, and barely touches CPU. Tune them for those
/// three, not for throughput.
///
/// The two caps are not redundant: they bind on disjoint sets of tables.
/// `max_rows` splits the *thin* tables, which would otherwise take a long time
/// to reach any byte threshold; `max_bytes` splits the *wide* ones, which reach
/// it almost immediately. Each therefore costs close to nothing on the tables
/// the other one reaches.
///
/// `max_bytes` bounds finalize wall-clock, which is what the streaming writer
/// exists to protect — a container gets on the order of ten seconds between
/// SIGTERM and SIGKILL, and an unsealed tail has to fit in it. A larger cap is
/// tempting because it produces fewer, denser segments, which shrinks the
/// archive and speeds queries (read cost tracks segment count); that trade
/// belongs to the offline compactor, which can have it without charging the
/// agent for it.
///
/// Going smaller is worse than it looks. Segments are the encoder's unit of
/// compression, so starving them re-pays per-column-chunk footer metadata on
/// every split and denies the RLE and dictionary encoders anything to amortize
/// over; well below this the archive inflates several-fold. 8 MiB is where that
/// curve has flattened and finalize has not yet climbed.
///
/// `max_rows` is what bounds the finalize tail on thin tables, and it is nearly
/// free precisely because it does not reach the wide ones.
impl Default for SealPolicy {
    fn default() -> Self {
        Self {
            max_bytes: 8 * 1024 * 1024,
            max_rows: 900,
            // Not a free variable like the two caps: this bounds how much an
            // unclean kill loses, not seal cost. Trade it against segment count.
            max_age: Duration::from_secs(300),
        }
    }
}

/// Everything the seal decision reads about an open segment, and none of the
/// rows.
///
/// **Separate from `TableBuilder` because only one of the two containers keeps
/// the rows.** v2 buffers them and encodes the builder it has been filling; v3
/// writes each row to the WAL and rebuilds the table from it at seal time, so
/// it has nothing to ask `rows()` or `approx_bytes()` of. Both must still seal
/// at the same row from the same input, which they do by both deciding here —
/// the alternative is two copies of a four-term predicate drifting apart in a
/// way that only shows up as differently-shaped archives.
pub(crate) struct SegmentAccount {
    rows: usize,
    approx_bytes: usize,
    /// Instant the current segment was opened (the age bound's origin).
    opened_at: Instant,
    max_rows: usize,
    max_age: Duration,
}

impl SegmentAccount {
    /// Open a sampler's **first** segment, with row and age targets reduced by
    /// a deterministic per-sampler fraction of up to 50%.
    ///
    /// This is a *phase offset*, not a period change. Every row-capped table
    /// otherwise advances exactly one row per tick starting from row 0, so they
    /// all reach `max_rows` in permanent lockstep and seal as one large batch
    /// forever. Co-seals, not large individual segments, are what put a seal
    /// over the tick budget. Shortening only the first segment desyncs the
    /// tables for the life of the recording while leaving steady-state segment
    /// size and count untouched — `rotate` restores the full policy.
    pub(crate) fn open_first(sampler: &str, policy: &SealPolicy) -> Self {
        let bucket = stagger_bucket(sampler);
        // Divide before multiplying: `max_rows` is `usize::MAX` in several
        // callers, and `max_rows * bucket` would overflow.
        let row_offset = (policy.max_rows / (2 * STAGGER_BUCKETS as usize)) * bucket as usize;
        let age_offset = (policy.max_age / (2 * STAGGER_BUCKETS as u32)) * bucket as u32;
        Self {
            rows: 0,
            approx_bytes: 0,
            opened_at: Instant::now(),
            // `max(1)` so a small policy can never yield a zero row target,
            // which would seal a one-row segment every tick forever.
            max_rows: policy.max_rows.saturating_sub(row_offset).max(1),
            max_age: policy.max_age.saturating_sub(age_offset),
        }
    }

    /// Account one appended row. `bytes` is [`entries_approx_bytes`] of that
    /// row, which is exactly what `TableBuilder::push_row` would have charged.
    pub(crate) fn add_row(&mut self, bytes: usize) {
        self.rows += 1;
        self.approx_bytes += bytes;
    }

    /// Whether this open segment is past any seal threshold. An empty segment
    /// never is.
    ///
    /// Row and age targets come from the account, not the policy: the first
    /// segment of each sampler is staggered short. The byte cap is a memory
    /// bound and is never staggered.
    pub(crate) fn is_due(&self, policy: &SealPolicy, now: Instant) -> bool {
        self.rows > 0
            && (self.approx_bytes >= policy.max_bytes
                || self.rows >= self.max_rows
                || now.duration_since(self.opened_at) >= self.max_age)
    }

    /// Reset onto a fresh segment after a seal, dropping the startup stagger:
    /// every segment after the first uses the full policy.
    pub(crate) fn rotate(&mut self, policy: &SealPolicy, now: Instant) {
        self.rows = 0;
        self.approx_bytes = 0;
        self.opened_at = now;
        self.max_rows = policy.max_rows;
        self.max_age = policy.max_age;
    }

    /// Rows in the open segment.
    pub(crate) fn rows(&self) -> usize {
        self.rows
    }

    /// The row and age targets the *current* open segment seals at.
    #[cfg(test)]
    pub(crate) fn targets(&self) -> (usize, Duration) {
        (self.max_rows, self.max_age)
    }
}

/// FNV-1a over the sampler name, reduced to a stagger bucket.
///
/// Hand-written rather than `DefaultHasher` on purpose: the offset must be
/// identical across runs, builds and Rust versions, and `DefaultHasher` is
/// SipHash with an explicitly unstable algorithm and no seed guarantee.
/// Randomizing the initial deadline would desync just as well, but a stable
/// offset keeps a recording's segment boundaries reproducible.
pub(crate) fn stagger_bucket(sampler: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64-bit offset basis
    for b in sampler.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3); // FNV-1a 64-bit prime
    }
    h % STAGGER_BUCKETS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offset must be reproducible across runs, builds and Rust versions,
    /// which is why it is a hand-written FNV-1a and not `DefaultHasher`. The
    /// literals pin the constants: if the hash changes, every recording's
    /// segment boundaries move.
    #[test]
    fn stagger_is_deterministic() {
        assert_eq!(stagger_bucket("cpu_usage"), 29);
        assert_eq!(stagger_bucket("scheduler"), 58);
        assert!((0..STAGGER_BUCKETS).contains(&stagger_bucket("anything_at_all")));
    }
}
