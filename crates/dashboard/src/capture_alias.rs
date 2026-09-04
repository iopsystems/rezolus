//! Naming the captures an A/B view is showing.

use std::collections::{BTreeMap, HashSet};

/// Pick the label that actually tells these recordings apart.
///
/// A fixed `arm`-then-`host` precedence breaks on the case `record` now makes
/// easy: two agents on ONE host differ only in `source`, so both arms would be
/// aliased to the same hostname and the A/B view could not be read. It breaks
/// the other way too — two hosts recorded without `--label` share
/// `source=rezolus`, so keying on `source` alone would collapse those.
///
/// So prefer `arm` when it discriminates, then `host`, then `source`, then any
/// other label that does; `None` means nothing distinguishes them and the
/// caller falls back to positional names. `--label` cannot help here: it
/// applies to every recording in a run, which is why the recorder warns when
/// two recordings end up with identical label sets.
///
/// **Shared by both viewers on purpose.** `rezolus view` and the static-site
/// WASM viewer both name their captures with this, so one archive is labelled
/// the same wherever it is opened. Two spellings of "which label tells these
/// apart" would be free to drift, and the symptom — one viewer showing
/// `web-01` where the other shows `baseline` — is the kind nobody reports.
pub fn discriminating_alias_key(recordings: &[BTreeMap<String, String>]) -> Option<String> {
    // Nothing to discriminate against. Without this, one recording makes every
    // predicate vacuously true and the first merely-PRESENT preferred key wins
    // — so a hindsight snapshot, whose labels are just `{source: rezolus}`,
    // would be aliased "rezolus" instead of the positional "baseline". An
    // empty slice is worse: `[].iter().all(..)` is true, so it would return
    // `Some("arm")` for no recordings at all.
    if recordings.len() < 2 {
        return None;
    }
    let discriminates = |k: &str| {
        let mut seen = HashSet::new();
        recordings
            .iter()
            .all(|labels| labels.get(k).is_some_and(|v| seen.insert(v.clone())))
    };
    const PREFERRED: [&str; 3] = ["arm", "host", "source"];
    PREFERRED
        .into_iter()
        .find(|k| discriminates(k))
        .map(str::to_string)
        .or_else(|| {
            let mut others: Vec<&String> = recordings
                .first()
                .map(|l| l.keys().collect())
                .unwrap_or_default();
            others.sort();
            others
                .into_iter()
                .find(|k| !PREFERRED.contains(&k.as_str()) && discriminates(k))
                .cloned()
        })
}

/// A capture's wire id and its display alias.
pub struct CaptureIdentity {
    /// The stable id used on the wire (`?capture=<id>`), as a map key, and in
    /// saved selections. Unique within one archive.
    pub id: String,
    /// What the UI shows for this capture — the discriminating label value, or
    /// the id when nothing distinguishes it.
    pub alias: String,
}

/// Assign a stable id and display alias to each capture, in DISPLAY ORDER
/// (the anchor first, its conventional A/B partner second, the rest after).
///
/// The one place the named-from-labels / positional-fallback wire-id scheme
/// lives, shared by both viewers so a `.rez` opened in the browser and from the
/// CLI names its captures identically — the same reason `discriminating_alias_key`
/// is shared.
///
/// - position 0 → `baseline`; position 1 → `experiment`. These keep the
///   wire-stable ids every existing URL and saved selection depends on.
/// - position k ≥ 2 → the discriminating label value when it is a safe, unused,
///   non-reserved token; otherwise positional `capture{k}`. So an archive with
///   distinguishing labels gets self-describing ids (`?capture=envoy`) while a
///   set of identically-labelled or bare-parquet captures still gets unique
///   ones.
///
/// The alias is the discriminating value whenever one exists — including for
/// `baseline`/`experiment`, so a `.rez` labelled `source=redis`/`source=valkey`
/// shows those names rather than the positional slot ids.
pub fn assign_capture_identities(
    order: &[BTreeMap<String, String>],
    all: &[BTreeMap<String, String>],
) -> Vec<CaptureIdentity> {
    // What a capture's alias must distinguish it from. Normally the recordings
    // shown alongside it (`order`); but a single recording shown out of a
    // larger archive is confused with the arms LEFT BEHIND, not with nothing —
    // aliasing it "baseline" under a window titled `fleet.rez` would say
    // nothing about which arm it is. So below two shown, discriminate against
    // the whole archive. (A label can tell a chosen PAIR apart while failing to
    // tell three apart, which is why the pair case stays scoped to `order`.)
    let context: &[BTreeMap<String, String>] = if order.len() < 2 && all.len() > 1 {
        all
    } else {
        order
    };
    let key = discriminating_alias_key(context);
    // A candidate id must be a clean wire/URL/JS-key token and must not poach a
    // reserved slot id or the positional namespace (`capture\d+`) — otherwise a
    // label value of "capture2" could alias position 2's fallback. The
    // `!taken` check is belt-and-braces: `discriminating_alias_key` only
    // returns a key whose values are all distinct, so two extras cannot share
    // one, but the id map must be collision-free no matter what feeds it.
    let safe = |v: &str| {
        !v.is_empty()
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
    };
    let reserved = |v: &str| {
        v == "baseline"
            || v == "experiment"
            || (v
                .strip_prefix("capture")
                .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())))
    };

    let mut taken: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(order.len());
    for (i, labels) in order.iter().enumerate() {
        let value = key.as_ref().and_then(|k| labels.get(k)).cloned();
        let id = match i {
            0 => "baseline".to_string(),
            1 => "experiment".to_string(),
            _ => value
                .as_deref()
                .filter(|v| safe(v) && !reserved(v) && !taken.contains(*v))
                .map(str::to_string)
                .unwrap_or_else(|| format!("capture{i}")),
        };
        taken.insert(id.clone());
        let alias = value.unwrap_or_else(|| id.clone());
        out.push(CaptureIdentity { id, alias });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::assign_capture_identities;

    fn recs(pairs: &[&[(&str, &str)]]) -> Vec<std::collections::BTreeMap<String, String>> {
        pairs
            .iter()
            .map(|rec| {
                rec.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .collect()
    }

    fn ids(order: &[std::collections::BTreeMap<String, String>]) -> Vec<(String, String)> {
        assign_capture_identities(order, order)
            .into_iter()
            .map(|c| (c.id, c.alias))
            .collect()
    }

    /// The first two keep the wire-stable slot ids; extras are named from the
    /// discriminating label. Aliases are the label value throughout.
    #[test]
    fn distinguishing_labels_give_named_ids_for_extras() {
        let order = recs(&[
            &[("source", "redis")],
            &[("source", "valkey")],
            &[("source", "envoy")],
        ]);
        assert_eq!(
            ids(&order),
            vec![
                ("baseline".into(), "redis".into()),
                ("experiment".into(), "valkey".into()),
                ("envoy".into(), "envoy".into()),
            ],
        );
    }

    /// No distinguishing label (identical sets) → positional ids, and the alias
    /// falls back to the id so the UI still has a name.
    #[test]
    fn identical_labels_fall_back_to_positional_ids() {
        let order = recs(&[
            &[("source", "rezolus")],
            &[("source", "rezolus")],
            &[("source", "rezolus")],
        ]);
        assert_eq!(
            ids(&order),
            vec![
                ("baseline".into(), "baseline".into()),
                ("experiment".into(), "experiment".into()),
                ("capture2".into(), "capture2".into()),
            ],
        );
    }

    /// A label value that would poach a reserved id or the positional namespace
    /// is refused, so no extra can collide with a slot id or another position.
    #[test]
    fn a_reserved_looking_label_value_does_not_poach_an_id() {
        let order = recs(&[
            &[("host", "a")],
            &[("host", "b")],
            &[("host", "experiment")],
            &[("host", "capture2")],
        ]);
        let got = ids(&order);
        // Extra 2's value "experiment" is reserved → positional capture2;
        // extra 3's value "capture2" is in the positional namespace → capture3.
        assert_eq!(got[2].0, "capture2");
        assert_eq!(got[3].0, "capture3");
        // Every id is unique.
        let mut all: Vec<&str> = got.iter().map(|(id, _)| id.as_str()).collect();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), got.len(), "ids must be unique: {got:?}");
    }

    /// A single recording shown out of a larger archive is still aliased by
    /// what tells it apart from the arms left behind — not the bare slot id.
    #[test]
    fn a_single_shown_recording_is_named_against_the_whole_archive() {
        let all = recs(&[
            &[("source", "redis")],
            &[("source", "valkey")],
            &[("source", "envoy")],
        ]);
        // Only the second recording is shown.
        let shown = vec![all[1].clone()];
        let got: Vec<(String, String)> = super::assign_capture_identities(&shown, &all)
            .into_iter()
            .map(|c| (c.id, c.alias))
            .collect();
        assert_eq!(got, vec![("baseline".to_string(), "valkey".to_string())]);
    }

    /// One and two recordings are unchanged: the slot ids, and no discriminator
    /// needed (nothing to distinguish against).
    #[test]
    fn one_and_two_recordings_use_the_slot_ids() {
        assert_eq!(ids(&recs(&[&[("source", "x")]]))[0].0, "baseline");
        let two = ids(&recs(&[&[("source", "redis")], &[("source", "valkey")]]));
        assert_eq!(two[0], ("baseline".to_string(), "redis".to_string()));
        assert_eq!(two[1], ("experiment".to_string(), "valkey".to_string()));
    }

    use super::discriminating_alias_key;
    use std::collections::BTreeMap;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn same_host_ab_aliases_off_the_label_that_differs() {
        // The case `record --endpoint :4241,source=redis --endpoint
        // :4242,source=valkey -o ab.rez` produces. Both recordings carry the
        // SAME host, so an arm-then-host precedence named both arms after the
        // host and the comparison could not be read. `--label` cannot fix it:
        // it applies to every recording in the run.
        let recs = vec![
            labels(&[("host", "alpha"), ("source", "redis")]),
            labels(&[("host", "alpha"), ("source", "valkey")]),
        ];
        assert_eq!(discriminating_alias_key(&recs).as_deref(), Some("source"));
    }

    #[test]
    fn multi_host_still_aliases_off_host() {
        // Two agents on different hosts, no --label: both are source=rezolus,
        // so keying on source would collapse them the other way.
        let recs = vec![
            labels(&[("host", "web-01"), ("source", "rezolus")]),
            labels(&[("host", "web-02"), ("source", "rezolus")]),
        ];
        assert_eq!(discriminating_alias_key(&recs).as_deref(), Some("host"));
    }

    #[test]
    fn arm_wins_when_it_discriminates() {
        let recs = vec![
            labels(&[("arm", "base"), ("host", "a"), ("source", "rezolus")]),
            labels(&[("arm", "exp"), ("host", "b"), ("source", "rezolus")]),
        ];
        assert_eq!(discriminating_alias_key(&recs).as_deref(), Some("arm"));
    }

    #[test]
    fn indistinguishable_recordings_fall_back_to_positional_names() {
        // The degenerate case the recorder warns about at startup.
        let recs = vec![
            labels(&[("host", "alpha"), ("source", "rezolus")]),
            labels(&[("host", "alpha"), ("source", "rezolus")]),
        ];
        assert_eq!(discriminating_alias_key(&recs), None);
    }

    #[test]
    fn a_single_recording_gets_no_alias_key() {
        // With one recording every predicate is vacuously true, so the first
        // merely-PRESENT preferred key would win. A hindsight snapshot's
        // labels are just `{source: rezolus}`, so it would be aliased
        // "rezolus" — strictly worse than the positional "baseline" it used to
        // get, and not a name at all.
        let recs = vec![labels(&[("source", "rezolus")])];
        assert_eq!(discriminating_alias_key(&recs), None);
        // Empty is worse still: `[].iter().all(..)` is true, so this returned
        // Some("arm") for no recordings at all.
        assert_eq!(discriminating_alias_key(&[]), None);
    }

    #[test]
    fn a_non_preferred_label_is_used_when_nothing_else_separates() {
        let recs = vec![
            labels(&[("host", "alpha"), ("source", "rezolus"), ("run", "1")]),
            labels(&[("host", "alpha"), ("source", "rezolus"), ("run", "2")]),
        ];
        assert_eq!(discriminating_alias_key(&recs).as_deref(), Some("run"));
    }
}
