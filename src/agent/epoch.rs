//! The agent's **producer epoch**: an opaque id that changes exactly when all
//! of this agent's cumulative counters restart from zero together.
//!
//! That happens once per process. The BPF maps are created when the agent
//! attaches its programs and are gone when it exits, so every counter this
//! agent exposes starts at zero on startup and counts monotonically until it
//! stops. A restart is therefore a reset of all of them at once, and nothing
//! short of a restart resets all of them at once.
//!
//! # Why a consumer needs it
//!
//! From the values alone, a counter that was reset and a counter that wrapped
//! are identical — both went down — while their arithmetic is not (`cur`
//! versus `cur + (2^w - prev)`). Every consumer in this ecosystem assumes
//! reset, which is right for a restart and silently wrong for an overflow.
//!
//! This id settles the *restart* half, for every counter at once and at
//! negligible cost. It does **not** settle the per-counter half: a counter
//! that wrapped, or that a sampler zeroes on read, did not restart the
//! process, so this id says nothing about it. That needs a generation per
//! counter, which is row data rather than process identity — see dendro's
//! `docs/journal/2026-09-12-generations-reset-versus-wrap.md`.
//!
//! There is also a case no value-based heuristic can catch, which is the
//! strongest argument for carrying this at all: a counter reset to zero that
//! counts past its previous value before the next observation shows **no drop
//! at all**. The series looks monotonic and the interval silently undercounts.
//! An epoch change is visible whether or not the value dropped.
//!
//! # Why random rather than derived
//!
//! A counter — "restart number 7" — would have to be persisted somewhere the
//! agent can write and would be wrong after any state loss. Host identity plus
//! a boot time would need a different source per platform and still collide
//! for two agents started in the same second on one host. A random id is
//! correct on every platform, needs no state, and collides with probability
//! that rounds to zero; ordering between epochs is not something a consumer
//! needs, since `producer_epochs` records them in observed order anyway.

use std::sync::OnceLock;

static EPOCH: OnceLock<String> = OnceLock::new();

/// This process's epoch, minted on first call and stable thereafter.
///
/// Callers must not cache it across a restart, which is the whole point: the
/// value is the identity of one run of one agent.
pub fn producer_epoch() -> &'static str {
    EPOCH.get_or_init(mint)
}

/// A random RFC 4122 version-4 UUID, formatted canonically.
///
/// Same shape dendro mints for a source, so the two are comparable by eye in a
/// manifest even though neither generates the other's.
fn mint() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        // The OS refused to give us 16 random bytes, which on a running system
        // means something is badly wrong. An epoch is still better than none:
        // a consumer needs a value that CHANGES across restarts, and the clock
        // plus the pid does that. It is not a v4 UUID and is not formatted as
        // one, so it cannot be mistaken for a real mint.
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        return format!("fallback-{ns:x}-{}", std::process::id());
    }
    // Version 4, variant 1, per RFC 4122 §4.4.
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stable within a process: two reads of the same run must agree, or a
    /// consumer would see an epoch change where no counter restarted.
    #[test]
    fn the_epoch_is_stable_within_a_process() {
        assert_eq!(producer_epoch(), producer_epoch());
    }

    /// Canonical v4 shape, so it is recognizable as a UUID wherever it lands.
    #[test]
    fn a_minted_epoch_is_a_canonical_v4_uuid() {
        let id = mint();
        assert_eq!(id.len(), 36, "{id}");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "{id}"
        );
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "{id}"
        );
        assert_eq!(parts[2].as_bytes()[0], b'4', "version nibble: {id}");
        assert!(
            matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'),
            "variant nibble: {id}"
        );
    }

    /// Two mints differ. The id exists to distinguish runs, so a generator
    /// that repeated itself would defeat the whole mechanism silently.
    #[test]
    fn two_mints_differ() {
        assert_ne!(mint(), mint());
    }
}
