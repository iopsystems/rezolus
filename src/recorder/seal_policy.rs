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

/// The stagger identity of a writer that can only ever hold one recording.
///
/// The V1/V2 tar writer is such a writer: a tar has no `recordings` table, so
/// there is never a second recording to desync against. Named rather than
/// spelled `""` at each call site so the reason is attached to the value.
///
/// Only reachable from test code today — the v2 writer survives as a fixture
/// builder for the reader and `parquet_tools` tests, and nothing in the bin's
/// live path constructs one any more.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const SINGLE_RECORDING_KEY: &str = "";

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
    max_bytes: usize,
    max_rows: usize,
    max_age: Duration,
}

impl SegmentAccount {
    /// Open a sampler's **first** segment, with byte, row and age targets
    /// reduced by a deterministic per-sampler fraction of up to 50%.
    ///
    /// **All three caps, not just rows and age.** The byte cap is the one that
    /// splits the *wide* tables (see `SealPolicy`), so leaving it unstaggered
    /// left exactly those tables with no phase offset at all. Within one
    /// recording that was survivable — different samplers fill at different
    /// rates, so they drift anyway — but two recordings of the SAME agent
    /// carry identical data, so a byte-bound table reached the cap on the same
    /// row in both and they sealed in permanent lockstep. Measured before the
    /// fix: `cpu_usage` 49/49 segment boundaries coincident across two
    /// recordings, against 1/6 for the row-bound tables.
    ///
    /// This is a *phase offset*, not a period change. Every row-capped table
    /// otherwise advances exactly one row per tick starting from row 0, so they
    /// all reach `max_rows` in permanent lockstep and seal as one large batch
    /// forever. Co-seals, not large individual segments, are what put a seal
    /// over the tick budget. Shortening only the first segment desyncs the
    /// tables for the life of the recording while leaving steady-state segment
    /// size and count untouched — `rotate` restores the full policy.
    pub(crate) fn open_first(sampler: &str, recording_key: &str, policy: &SealPolicy) -> Self {
        let bucket = stagger_bucket(sampler, recording_key);
        // Divide before multiplying: `max_rows` is `usize::MAX` in several
        // callers, and `max_rows * bucket` would overflow.
        let row_offset = (policy.max_rows / (2 * STAGGER_BUCKETS as usize)) * bucket as usize;
        let age_offset = (policy.max_age / (2 * STAGGER_BUCKETS as u32)) * bucket as u32;
        let byte_offset = (policy.max_bytes / (2 * STAGGER_BUCKETS as usize)) * bucket as usize;
        Self {
            rows: 0,
            approx_bytes: 0,
            opened_at: Instant::now(),
            // `max(1)` for the same reason as the row target: a zero cap would
            // seal an empty segment every tick forever.
            max_bytes: policy.max_bytes.saturating_sub(byte_offset).max(1),
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
    /// Takes no policy: the account carries its own copy of all three caps,
    /// because all three are staggered on the first segment and `rotate`
    /// restores them. Reading `policy.max_bytes` here instead is what let the
    /// byte cap escape the stagger.
    pub(crate) fn is_due(&self, now: Instant) -> bool {
        self.rows > 0
            && (self.approx_bytes >= self.max_bytes
                || self.rows >= self.max_rows
                || now.duration_since(self.opened_at) >= self.max_age)
    }

    /// Reset onto a fresh segment after a seal, dropping the startup stagger:
    /// every segment after the first uses the full policy.
    pub(crate) fn rotate(&mut self, policy: &SealPolicy, now: Instant) {
        self.rows = 0;
        self.approx_bytes = 0;
        self.opened_at = now;
        self.max_bytes = policy.max_bytes;
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

    /// This segment's byte cap, after the first-segment stagger.
    #[cfg(test)]
    pub(crate) fn byte_target(&self) -> usize {
        self.max_bytes
    }
}

/// FNV-1a over the sampler name AND the recording's identity, reduced to a
/// stagger bucket.
///
/// Hand-written rather than `DefaultHasher` on purpose: the offset must be
/// identical across runs, builds and Rust versions, and `DefaultHasher` is
/// SipHash with an explicitly unstable algorithm and no seed guarantee.
/// Randomizing the initial deadline would desync just as well, but a stable
/// offset keeps a recording's segment boundaries reproducible.
///
/// **`recording_key` is why this is not just the sampler name.** An archive can
/// hold several recordings, and two rezolus agents have *identical* sampler
/// sets — so keying on the sampler alone would give every table in recording B
/// the same bucket as its namesake in A. The two recordings would then seal in
/// permanent lockstep, doubling the co-seal batch size exactly when the archive
/// holds twice the tables: the stagger still working within a recording and
/// silently defeated across them.
///
/// The key is the recording's canonical label set, not its `recordings` row id.
/// An autoincrement id would make the bucket — and so where every segment
/// boundary falls — depend on the order endpoints were listed on the command
/// line, and the same two agents recorded with the flags swapped would segment
/// differently for no reason. Labels are stable across runs and across flag
/// order, and they are what actually distinguishes two arms: `host` separates a
/// multi-host archive, and an A/B on a *single* host separates only on `arm`,
/// which a node name alone would miss.
///
/// Two recordings with genuinely identical label sets still collide. That is
/// the degenerate case — the operator gave two endpoints nothing to tell them
/// apart — and the answer is to warn rather than to fold in the id and
/// reintroduce order-dependence.
pub(crate) fn stagger_bucket(sampler: &str, recording_key: &str) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3; // FNV-1a 64-bit prime

    // Absorb one byte, twice: the byte itself, then the two bits the final
    // `% STAGGER_BUCKETS` would otherwise discard.
    //
    // Plain FNV-1a reduced mod 64 depends only on `byte & 0x3f`, because the
    // prime is odd and so `x -> ((x ^ b) * PRIME) mod 64` is a bijection on
    // Z64 keyed on the low six bits. Two keys that agree byte-for-byte modulo
    // 0x40 therefore draw the SAME bucket for every sampler — total lockstep,
    // the exact failure this stagger exists to prevent. The aliasing pairs are
    // ordinary in hostnames: `-` with `m`, `.` with `n`, digits with `p`-`y`.
    // `host=web-01` and `host=webm01` collided on all of them.
    //
    // Folding the high bits in as their own absorbed value breaks that
    // identity for bits 6-7 while leaving the low-bit structure — which
    // measures *better* than a well-avalanched finalizer here — intact.
    //
    // It does NOT close bit 5, and the same algebra applies: `x ^ 0x20` is
    // `x + 32 (mod 64)` and `51 * 32 == 32 (mod 64)`, so flipping bit 5 of an
    // absorbed byte just XORs 0x20 through the whole chain. Two label sets
    // differing by an EVEN number of bit-5 flips still share every bucket. In
    // printable ASCII bit 5 is the case bit, so this needs two recordings
    // whose labels differ only by capitalisation (`host=Web-01` vs
    // `host=weB-01`) — which within one `record` run means an operator typing
    // two `source=` values that differ only in case. Left open rather than
    // absorbing `b >> 5` as well, because each extra fold costs some of the
    // low-bit advantage and this class is far narrower than the one closed.
    // Tracked in docs/backlog.md.
    let absorb = |h: &mut u64, b: u64| {
        *h ^= b;
        *h = h.wrapping_mul(PRIME);
        *h ^= b >> 6;
        *h = h.wrapping_mul(PRIME);
    };

    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64-bit offset basis
    for b in sampler.as_bytes() {
        absorb(&mut h, *b as u64);
    }
    // A separator no label byte can supply, so `a=1,b=2` and `a=1,b=2` reached
    // from different splits cannot alias.
    absorb(&mut h, 0xff);
    for b in recording_key.as_bytes() {
        absorb(&mut h, *b as u64);
    }
    h % STAGGER_BUCKETS
}

/// A recording's stagger identity: its label set, canonically rendered.
///
/// `BTreeMap` already fixes the order, so this is just a rendering — but it is
/// done in one place so the writer and any test agree on the exact bytes.
pub(crate) fn recording_stagger_key(labels: &std::collections::BTreeMap<String, String>) -> String {
    let mut out = String::new();
    for (k, v) in labels {
        if !out.is_empty() {
            out.push('\u{1}');
        }
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    out
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
        assert_eq!(stagger_bucket("cpu_usage", SINGLE_RECORDING_KEY), 32);
        assert_eq!(stagger_bucket("scheduler", SINGLE_RECORDING_KEY), 19);
        assert!(
            (0..STAGGER_BUCKETS).contains(&stagger_bucket("anything_at_all", SINGLE_RECORDING_KEY))
        );
    }

    /// Keys that differ only in the bits `% STAGGER_BUCKETS` discards must
    /// still separate.
    ///
    /// Plain FNV-1a reduced mod 64 depends only on each byte's low six bits,
    /// so two hostnames agreeing byte-for-byte modulo 0x40 drew the same
    /// bucket for EVERY sampler — complete lockstep between two recordings
    /// that look nothing alike. The pairs are ordinary: `-`/`m`, `.`/`n`,
    /// digits against `p`-`y`.
    #[test]
    fn hosts_that_alias_in_the_low_bits_still_desync() {
        let key = |host: &str| {
            recording_stagger_key(
                &[
                    ("host".to_string(), host.to_string()),
                    ("source".to_string(), "rezolus".to_string()),
                ]
                .into_iter()
                .collect(),
            )
        };
        let samplers = [
            "cpu_usage",
            "scheduler",
            "blockio_latency",
            "tcp_traffic",
            "syscall_latency",
            "cpu_bandwidth",
        ];
        // Each pair differs only in bits 6-7 of one byte.
        for (a, b) in [
            ("web-01", "webm01"),
            ("node1", "nodeq"),
            ("web.01", "webn01"),
        ] {
            let (ka, kb) = (key(a), key(b));
            let collisions = samplers
                .iter()
                .filter(|s| stagger_bucket(s, &ka) == stagger_bucket(s, &kb))
                .count();
            assert_eq!(
                collisions,
                0,
                "{a} and {b} share {collisions} of {} buckets — the reduction is \
                 discarding the bits that separate them. `collisions < len` would be \
                 too weak a bar here: 5 of 6 coincident is still the lockstep this \
                 test exists to catch",
                samplers.len()
            );
        }
    }

    /// The reason the key widened past the sampler name.
    ///
    /// Two rezolus agents have identical sampler sets. Keyed on the sampler
    /// alone, every table in one recording drew its namesake's bucket in the
    /// other, so both sealed in permanent lockstep — doubling the co-seal batch
    /// exactly when the archive holds twice the tables. This is the assertion
    /// that fails if the recording key is ever dropped from the hash.
    #[test]
    fn two_recordings_do_not_share_a_samplers_bucket() {
        let a = recording_stagger_key(
            &[("host".to_string(), "alpha".to_string())]
                .into_iter()
                .collect(),
        );
        let b = recording_stagger_key(
            &[("host".to_string(), "beta".to_string())]
                .into_iter()
                .collect(),
        );

        // Every sampler the two hosts share must land somewhere different.
        let shared = ["cpu_usage", "scheduler", "blockio_latency", "tcp_traffic"];
        let collisions = shared
            .iter()
            .filter(|s| stagger_bucket(s, &a) == stagger_bucket(s, &b))
            .count();
        assert_eq!(
            collisions, 0,
            "identical sampler sets must not draw identical buckets across recordings"
        );
    }

    /// The byte cap must be staggered too, not just rows and age.
    ///
    /// This is the one the measurement caught. The byte cap splits the *wide*
    /// tables, so leaving it on the shared policy left exactly those tables
    /// with no phase offset: two recordings of one agent carry identical data,
    /// reach the cap on the same row, and seal together forever. Measured
    /// before the fix, `cpu_usage` had 49 of 49 segment boundaries coincident
    /// across two recordings; after, 1 of 5.
    #[test]
    fn the_byte_cap_is_staggered_across_recordings() {
        let policy = SealPolicy {
            max_bytes: 8 * 1024 * 1024,
            max_rows: 900,
            max_age: Duration::from_secs(300),
        };
        let key = |host: &str| {
            recording_stagger_key(
                &[("host".to_string(), host.to_string())]
                    .into_iter()
                    .collect(),
            )
        };

        // Drive the seal, don't just compare the field. The bug was never a
        // missing field — it was `is_due` reading `policy.max_bytes` instead
        // of the account's own copy, which a field comparison cannot see.
        let mut a = SegmentAccount::open_first("cpu_usage", &key("alpha"), &policy);
        let mut b = SegmentAccount::open_first("cpu_usage", &key("beta"), &policy);
        let now = Instant::now();
        // Rows wide enough that the byte cap is what fires, well before
        // `max_rows`: 8 MiB / 900 rows is ~9 KiB per row, so 64 KiB rows are
        // byte-bound by construction.
        let mut split = None;
        for row in 1..=policy.max_rows {
            a.add_row(64 * 1024);
            b.add_row(64 * 1024);
            if a.is_due(now) != b.is_due(now) {
                split = Some(row);
                break;
            }
            // Checked only once they agree, so an unstaggered byte cap falls
            // through to the `split.is_some()` assertion below with its
            // accurate message rather than tripping this one first.
            assert!(
                row < policy.max_rows,
                "the byte cap must fire before the row cap, or this tests the wrong cap"
            );
        }
        assert!(
            split.is_some(),
            "both recordings' byte-bound tables sealed on the same row — the byte \
             cap escaped the stagger"
        );

        // And the stagger stays inside its documented bound: at most 50% off,
        // never zero.
        for host in ["alpha", "beta", "gamma"] {
            let acct = SegmentAccount::open_first("cpu_usage", &key(host), &policy);
            assert!(acct.byte_target() > policy.max_bytes / 2 - 1);
            assert!(acct.byte_target() <= policy.max_bytes);
        }
    }

    /// An A/B on ONE host separates only on `arm` — which is why the key is the
    /// whole label set and not the node name.
    #[test]
    fn same_host_different_arms_still_desync() {
        let base = |arm: &str| {
            recording_stagger_key(
                &[
                    ("host".to_string(), "alpha".to_string()),
                    ("arm".to_string(), arm.to_string()),
                ]
                .into_iter()
                .collect(),
            )
        };
        let (a, b) = (base("redis"), base("valkey"));
        assert_ne!(a, b, "the arm label must reach the key");
        assert_ne!(
            stagger_bucket("cpu_usage", &a),
            stagger_bucket("cpu_usage", &b)
        );
    }

    /// The bucket follows a recording's labels, not its position.
    ///
    /// This is deliberately NOT written as "compute the pair in both orders
    /// and compare". `stagger_bucket` takes two `&str` and no index, and
    /// `recording_stagger_key` takes a `BTreeMap` that is sorted before it is
    /// called — so any such assertion reduces to `[f(a), f(b)] == [f(a),
    /// f(b))]`, two calls to a pure function compared with themselves. It
    /// cannot fail for any implementation, which is exactly what was wrong
    /// with the version this replaces.
    ///
    /// Order-independence is real, but it is a property of the *layer above*:
    /// the recording id — the thing that would have made segmentation depend
    /// on endpoint order — never reaches this function, which is the design
    /// decision itself. `stagger_key_follows_the_labels_not_the_open_order`
    /// in `rez_v3_writer` pins it where the id exists. What is left to assert
    /// here is the content: two arms must land in different buckets.
    #[test]
    fn the_two_arms_of_an_ab_land_in_different_buckets() {
        let key = |arm: &str| {
            recording_stagger_key(
                &[
                    ("host".to_string(), "alpha".to_string()),
                    ("arm".to_string(), arm.to_string()),
                ]
                .into_iter()
                .collect(),
            )
        };
        assert_ne!(
            stagger_bucket("cpu_usage", &key("redis")),
            stagger_bucket("cpu_usage", &key("valkey"))
        );
    }
}
