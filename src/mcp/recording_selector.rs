//! Selecting one recording out of a multi-recording `.rez` archive.
//!
//! Pure logic over label sets: no file access, no reader. Kept separate from
//! `mcp/mod.rs` because this is the part most worth testing directly, and
//! because `mod.rs` is already large.
//!
//! `resolve`, `matches`, `is_empty`, `Display` and `describe_candidates` are
//! all driven from `mcp::open_source_with_pool`, `parse` from the
//! `--recording` CLI flag, and `from_json` from the stdio server's `recording`
//! tool argument.

use std::collections::BTreeMap;
use std::fmt;

/// Labels a recording must carry to be selected.
///
/// Subset semantics: every `k=v` here must appear in the recording's labels,
/// but the recording may carry more. Recordings auto-carry `source` and
/// `host` plus any `record --label`, so requiring the full set would make
/// selectors long and brittle.
// `Hash` so a selector can be half of a cache key as a TYPED pair. The stdio
// server caches one reader per (path, selector); formatting the selector into
// a string key instead would let two different selectors collide, since label
// values are free text and `{"a": "b,c=d"}` renders the same `Display` text as
// `{"a": "b", "c": "d"}` — and a collided key hands back a reader for the
// wrong arm, silently.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct RecordingSelector {
    // `BTreeMap`, not `HashMap`: `Display` renders selector text into error
    // messages, so iteration order has to be deterministic (pinned by
    // `display_orders_pairs_deterministically` below); a `BTreeMap` also
    // matches `rez_sqlite::RecordingMeta.labels`, so a manifest's labels can
    // be consumed with no conversion.
    labels: BTreeMap<String, String>,
}

/// Why a selector did not name exactly one recording.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SelectError {
    /// No recording carries every label in the selector.
    NoMatch,
    /// Several do; the indices of those that matched.
    Ambiguous(Vec<usize>),
}

impl RecordingSelector {
    /// Parse repeated `k=v` arguments (the CLI shape).
    ///
    /// A key must be non-empty and appear at most once: this is the one
    /// module whose entire job is refusing to resolve ambiguity silently, so
    /// letting `--recording host=a --recording host=b` collapse to `host=b`
    /// via last-write-wins would be that same failure creeping in one layer
    /// earlier. A value may itself contain `=` (`split_once` splits on the
    /// first one only), which is deliberate: label values are free text.
    pub(crate) fn parse(pairs: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut labels = BTreeMap::new();
        for p in pairs {
            let (k, v) = p
                .split_once('=')
                .ok_or_else(|| format!("--recording expects key=value, got {p:?} with no '='"))?;
            if k.is_empty() {
                return Err(format!(
                    "--recording expects key=value, got {p:?} with an empty key"
                ));
            }
            if let Some(prev) = labels.insert(k.to_string(), v.to_string()) {
                return Err(format!(
                    "--recording {k}= specified more than once ({prev:?} and {v:?})"
                ));
            }
        }
        Ok(Self { labels })
    }

    /// Parse a JSON object of label key to value (the stdio-server shape).
    pub(crate) fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let obj = v
            .as_object()
            .ok_or_else(|| "recording must be an object of label key to value".to_string())?;
        let mut labels = BTreeMap::new();
        for (k, val) in obj {
            let s = val
                .as_str()
                .ok_or_else(|| format!("recording label {k:?} must be a string"))?;
            labels.insert(k.clone(), s.to_string());
        }
        Ok(Self { labels })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    /// The selector as the CALLER would actually spell it, in `syntax`.
    ///
    /// NOT `Display`, and the difference matters. `Display` joins with `,`
    /// because it renders a selector as one readable token, but `parse` splits
    /// each argument on its FIRST `=` only, so pasting `--recording
    /// host=b,source=a` back parses as the single label `host` =
    /// `"b,source=a"`, matches nothing, and fails as `NoMatch` with no hint
    /// why. Error text that invites the reader to adjust and retry a selector
    /// must therefore print a form that round-trips through the front end it
    /// is being shown to.
    pub(crate) fn render(&self, syntax: SelectorSyntax) -> String {
        picker_form(
            self.labels.iter().map(|(k, v)| (k.clone(), v.clone())),
            syntax,
        )
    }

    /// Whether `labels` carries every pair in this selector.
    pub(crate) fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.labels
            .iter()
            .all(|(k, v)| labels.get(k).is_some_and(|got| got == v))
    }

    /// The index of the single recording this selector names.
    ///
    /// An EMPTY selector matches every candidate, so with several recordings
    /// it resolves to `Ambiguous` — "no selector given" and "an ambiguous
    /// selector" reach the caller through one path, and there is a single
    /// place that decides what to say.
    pub(crate) fn resolve(
        &self,
        candidates: &[BTreeMap<String, String>],
    ) -> Result<usize, SelectError> {
        let hits: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, l)| self.matches(l))
            .map(|(i, _)| i)
            .collect();
        match hits.as_slice() {
            [] => Err(SelectError::NoMatch),
            [one] => Ok(*one),
            _ => Err(SelectError::Ambiguous(hits)),
        }
    }
}

impl fmt::Display for RecordingSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rendered: Vec<String> = self
            .labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        write!(f, "{}", rendered.join(","))
    }
}

/// How the caller being spoken to spells a recording selector.
///
/// The two front ends do not share a syntax: the one-shot CLI takes a
/// repeatable `--recording k=v` flag, while an MCP client has no flags at all
/// and sends a `recording` object in the tool call's arguments. Every message
/// that tells a caller how to name an arm has to be rendered for the front
/// end that will read it — a listing that hands an agent `--recording
/// source=redis` is an instruction it cannot follow, and `describe_recording`
/// (whose schema nominates it as the discovery step) is exactly where that
/// dead end lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectorSyntax {
    /// `--recording k=v --recording k=v` — the one-shot CLI subcommands.
    Flags,
    /// `recording {"k": "v"}` — the stdio server's tool argument.
    Json,
}

/// Render `k=v` pairs as the selector a caller of `syntax` would type.
///
/// One spelling for both the listing's "select with" lines and the error
/// leads that echo a selector back, because the two must agree. The flag form
/// repeats the flag rather than joining with `Display`'s comma, which does
/// not survive a round trip through `parse`; the JSON form is built through
/// `serde_json` so a label value containing a quote or a backslash still
/// pastes back as valid JSON.
fn picker_form(
    pairs: impl IntoIterator<Item = (String, String)>,
    syntax: SelectorSyntax,
) -> String {
    let pairs: Vec<(String, String)> = pairs.into_iter().collect();
    match syntax {
        SelectorSyntax::Flags => format!(
            "--recording {}",
            pairs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" --recording ")
        ),
        SelectorSyntax::Json => format!(
            "recording {{{}}}",
            pairs
                .iter()
                .map(|(k, v)| format!("{}: {}", json_str(k), json_str(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// One JSON string literal, escaped.
fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// Render a recording's labels for display: `k=v, k=v` in key order.
///
/// Not the flag form — this is prose, naming a recording inside a sentence,
/// so it must not look like something to paste.
pub(crate) fn render_labels(labels: &BTreeMap<String, String>) -> String {
    labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render specific recordings from an archive, each with the selector that
/// picks it, spelled in `syntax`.
///
/// `all` is every recording's labels; `highlight` is which indices of `all`
/// to render (an empty slice means "render all of them"). Uniqueness is
/// always computed against `all`, never against `highlight` — that split is
/// deliberate and prevents a real bug: describing only a matched subset
/// (e.g. the indices `resolve` returned inside `Ambiguous`) using that
/// subset as the uniqueness universe would call a label unique because it
/// only looks that way among the couple of recordings being described, then
/// hand back a selector that still collides with a THIRD recording
/// sitting elsewhere in the archive. Taking `all` separately from
/// `highlight` makes that mistake impossible to write.
///
/// Printing the selector rather than only the labels is the point: the
/// caller's next command is a copy of a line it was just given.
///
/// Deliberately unnumbered — an index would invite `--recording 1`, which is
/// not a supported selector.
pub(crate) fn describe_candidates(
    all: &[BTreeMap<String, String>],
    highlight: &[usize],
    syntax: SelectorSyntax,
) -> String {
    let indices: Vec<usize> = if highlight.is_empty() {
        (0..all.len()).collect()
    } else {
        highlight.to_vec()
    };

    let mut out = String::new();
    for i in indices {
        let labels = &all[i];
        let rendered = render_labels(labels);

        let Some(pairs) = picker_pairs(all, i) else {
            // Wrapped by hand, with the indent interpolated: this lands in
            // terminal output and in an MCP tool result, neither of which
            // re-wraps, and one 300-column line is what an agent then quotes
            // back at a user.
            const IND: &str = "    ";
            out.push_str(&format!(
                "  - {rendered}\n\
                 {IND}cannot be selected by labels — every selector that names it also \
                 names another\n\
                 {IND}recording in this archive. To tell them apart, give them distinct \
                 labels: re-capture\n\
                 {IND}giving each --endpoint its own source=NAME, or — for an archive \
                 built by `rezolus\n\
                 {IND}recording combine` — re-record its inputs with `record --label \
                 arm=NAME`.\n"
            ));
            continue;
        };

        out.push_str(&format!(
            "  - {rendered}\n    select with: {}\n",
            picker_form(pairs, syntax)
        ));
    }
    out
}

/// Whether any recording in `all` can be named by a selector at all.
///
/// Asked by a caller that is about to print a listing and needs to know
/// whether it will contain any "select with:" lines — text inviting the
/// reader to use one is a lie when every recording is indistinguishable from
/// a peer. Shares `picker_pairs` with the listing itself rather than
/// re-deriving the rule, so the two cannot disagree.
pub(crate) fn any_selectable(all: &[BTreeMap<String, String>]) -> bool {
    (0..all.len()).any(|i| picker_pairs(all, i).is_some())
}

/// The `k=v` pairs that pick recording `i` out of `all`, or `None` when no
/// selector can.
///
/// The one place the "is this recording selectable at all" rule lives, so a
/// listing and any caller asking whether a listing will contain selectors
/// cannot disagree.
fn picker_pairs(all: &[BTreeMap<String, String>], i: usize) -> Option<Vec<(String, String)>> {
    let labels = &all[i];

    // A recording is unselectable when some OTHER recording's labels are a
    // SUPERSET of its own, because `matches` is subset semantics: every
    // selector that matches `L` also matches `L ∪ {extra}`, so nothing
    // singles out the smaller set. Testing plain equality here (the original
    // rule) covered only the identical-labels case and let the superset case
    // fall through to the whole-label-set fallback below, which then printed
    // a selector resolving to `Ambiguous` — a closed loop, since the caller
    // is told to add labels and has none left to add. Reached by the
    // documented A/B workflow (`--label arm=baseline` on one capture, none on
    // the other, then `recording combine`) and by a single multi-endpoint run
    // where one endpoint's `/systeminfo` fetch fails, dropping `host` from
    // that arm alone. Equality is a special case of superset, so this
    // subsumes the original rule.
    //
    // The `is_empty` term is not subsumed: it is the only cover for a LONE
    // unlabeled recording, where no other recording exists to be a superset
    // and the fallback would emit a bare `--recording` with no pair at all.
    // Echoes the recorder's own `warn_if_indistinguishable`, so both messages
    // point at the same fix.
    if labels.is_empty()
        || all.iter().enumerate().any(|(j, c)| {
            j != i
                && labels
                    .iter()
                    .all(|(k, v)| c.get(k).is_some_and(|got| got == v))
        })
    {
        return None;
    }

    // Any single label unique to this recording (checked against `all`, not
    // just `highlight`) selects it on its own; take the first in key order so
    // the listing is byte-stable across runs. If none exists, fall back to
    // the whole set, whose pairs the selector ANDs together — sound here
    // precisely because no other
    // recording is a superset.
    let unique = labels
        .iter()
        .filter(|(k, v)| {
            all.iter()
                .filter(|c| c.get(*k).is_some_and(|got| got == *v))
                .count()
                == 1
        })
        .map(|(k, v)| vec![(k.clone(), v.clone())])
        .next();
    Some(unique.unwrap_or_else(|| labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_key_value_pairs() {
        let s = RecordingSelector::parse(["source=redis".to_string()]).unwrap();
        assert!(!s.is_empty());
        assert_eq!(s.to_string(), "source=redis");
    }

    #[test]
    fn a_value_without_an_equals_is_an_error() {
        // Not silently ignored: `--recording redis` is a plausible typo for
        // `--recording source=redis`, and dropping it would run against the
        // wrong recording rather than say so.
        let err = RecordingSelector::parse(["redis".to_string()]).unwrap_err();
        assert!(
            err.contains("redis"),
            "the error must quote the input: {err}"
        );
        assert!(err.contains('='), "and say what was expected: {err}");
    }

    #[test]
    fn an_empty_key_is_an_error() {
        // `--recording =redis` is a typo (probably for `source=redis`), not
        // a label named "". Left unchecked it would silently match no
        // recording ever, since `matches` looks the empty key up on real
        // label maps and never finds it, so the failure would surface as
        // "no recording matches" instead of "that isn't a key=value".
        let err = RecordingSelector::parse(["=redis".to_string()]).unwrap_err();
        assert!(
            err.contains("=redis"),
            "the error must quote the input: {err}"
        );
        assert!(err.contains("empty key"), "{err}");
    }

    #[test]
    fn an_empty_value_is_fine() {
        // Unlike an empty key, an empty value is a legitimate label value,
        // not a typo, so it must not be rejected the same way.
        let s = RecordingSelector::parse(["source=".to_string()]).unwrap();
        assert!(s.matches(&labels(&[("source", "")])));
    }

    #[test]
    fn a_repeated_key_is_an_error() {
        // Silently taking the last value would be the exact ambiguity this
        // module exists to refuse, just resolved one step before `resolve`
        // ever runs.
        let err =
            RecordingSelector::parse(["host=a".to_string(), "host=b".to_string()]).unwrap_err();
        assert!(err.contains("host"), "{err}");
        assert!(err.contains('a') && err.contains('b'), "{err}");
    }

    #[test]
    fn a_value_may_itself_contain_an_equals_sign() {
        // `split_once` (not `split('=')`) is deliberate: a label value is
        // free text and may legitimately contain '='. Pinned here so a
        // future "simplify this" pass doesn't swap it and silently truncate
        // values like this one.
        let s = RecordingSelector::parse(["label=a=b".to_string()]).unwrap();
        assert!(s.matches(&labels(&[("label", "a=b")])));
    }

    #[test]
    fn an_empty_selector_matches_everything() {
        let s = RecordingSelector::default();
        assert!(s.is_empty());
        // An empty selector matches everything, which is what makes
        // "no selector given" fall through to the caller's own policy
        // rather than being a special case inside the matcher.
        assert!(s.matches(&labels(&[("source", "redis")])));
    }

    #[test]
    fn matching_is_a_subset_not_an_equality() {
        let s = RecordingSelector::parse(["source=redis".to_string()]).unwrap();
        assert!(s.matches(&labels(&[("source", "redis"), ("host", "web-01")])));
    }

    #[test]
    fn a_differing_value_does_not_match() {
        let s = RecordingSelector::parse(["source=redis".to_string()]).unwrap();
        assert!(!s.matches(&labels(&[("source", "valkey")])));
    }

    #[test]
    fn a_missing_key_does_not_match() {
        let s = RecordingSelector::parse(["arm=a".to_string()]).unwrap();
        assert!(!s.matches(&labels(&[("source", "redis")])));
    }

    #[test]
    fn several_pairs_are_anded() {
        let s = RecordingSelector::parse(["source=redis".to_string(), "host=web-01".to_string()])
            .unwrap();
        assert!(s.matches(&labels(&[("source", "redis"), ("host", "web-01")])));
        assert!(!s.matches(&labels(&[("source", "redis"), ("host", "web-02")])));
    }

    #[test]
    fn display_orders_pairs_deterministically() {
        // Not just "some order" — swapping the backing map for a `HashMap`
        // must not pass this test, since `Display` output lands in error
        // messages that need to be reproducible.
        let s = RecordingSelector::parse(["host=b".to_string(), "source=a".to_string()]).unwrap();
        assert_eq!(s.to_string(), "host=b,source=a");
    }

    /// Read a rendered selector back the way its front end would.
    ///
    /// The flag form goes through `parse` (what clap hands over); the JSON
    /// form through `serde_json` + `from_json` (what an MCP client sends).
    /// Anything a message prints has to survive this, or the caller is being
    /// told to type something that does not work.
    fn reparse(rendered: &str, syntax: SelectorSyntax) -> RecordingSelector {
        match syntax {
            SelectorSyntax::Flags => RecordingSelector::parse(
                rendered
                    .strip_prefix("--recording ")
                    .unwrap_or_else(|| panic!("not a flag-form selector: {rendered:?}"))
                    .split(" --recording ")
                    .map(str::to_string),
            )
            .unwrap_or_else(|e| panic!("unparseable flag selector {rendered:?}: {e}")),
            SelectorSyntax::Json => {
                let body = rendered
                    .strip_prefix("recording ")
                    .unwrap_or_else(|| panic!("not a JSON-form selector: {rendered:?}"));
                let v: serde_json::Value = serde_json::from_str(body)
                    .unwrap_or_else(|e| panic!("invalid JSON selector {body:?}: {e}"));
                RecordingSelector::from_json(&v)
                    .unwrap_or_else(|e| panic!("unusable JSON selector {body:?}: {e}"))
            }
        }
    }

    #[test]
    fn the_flag_form_round_trips_where_display_does_not() {
        let s = RecordingSelector::parse(["host=b".to_string(), "source=a".to_string()]).unwrap();
        assert_eq!(
            s.render(SelectorSyntax::Flags),
            "--recording host=b --recording source=a"
        );

        // The reason `render` exists rather than reusing `Display`: the
        // comma join reads well but does NOT survive `parse`, which splits on
        // the first `=` only, so it comes back as one label
        // `host="b,source=a"` that matches nothing. Error text tells the
        // caller to adjust and retry a selector, so it must print the form
        // that round-trips.
        let pasted = RecordingSelector::parse([s.to_string()]).unwrap();
        assert_ne!(pasted, s, "Display must not be mistaken for pasteable");

        assert_eq!(
            reparse(&s.render(SelectorSyntax::Flags), SelectorSyntax::Flags),
            s,
            "the flag form must round-trip through parse"
        );
    }

    /// The stdio server's caller has no flags to type: it sends a `recording`
    /// object in the tool call. A message rendered for it must therefore be
    /// JSON that survives `from_json`, not CLI syntax.
    #[test]
    fn the_json_form_round_trips_through_the_server_parser() {
        let s = RecordingSelector::parse(["host=b".to_string(), "source=a".to_string()]).unwrap();
        let rendered = s.render(SelectorSyntax::Json);
        assert_eq!(rendered, r#"recording {"host": "b", "source": "a"}"#);
        assert!(
            !rendered.contains("--recording"),
            "an MCP client has no flag to type: {rendered}"
        );
        assert_eq!(reparse(&rendered, SelectorSyntax::Json), s);
    }

    /// A label value carrying a quote or a backslash must still paste back as
    /// valid JSON — hence rendering through `serde_json` rather than
    /// `format!`. Label values are free text (a container name, a git
    /// revision, a command line), so this is not hypothetical.
    #[test]
    fn the_json_form_escapes_label_values() {
        let s = RecordingSelector::parse([r#"cmd=say "hi"\now"#.to_string()]).unwrap();
        let rendered = s.render(SelectorSyntax::Json);
        assert_eq!(reparse(&rendered, SelectorSyntax::Json), s);
    }

    #[test]
    fn parses_a_json_object() {
        let v = serde_json::json!({"source": "redis"});
        let s = RecordingSelector::from_json(&v).unwrap();
        assert_eq!(s.to_string(), "source=redis");
    }

    #[test]
    fn a_non_object_json_selector_is_an_error() {
        let err = RecordingSelector::from_json(&serde_json::json!("redis")).unwrap_err();
        assert!(err.contains("object"), "{err}");
    }

    #[test]
    fn a_non_string_json_value_is_an_error() {
        // An LLM-driven MCP client emitting a bare number for a
        // numeric-looking label (`{"arm": 1}`) is realistic; the error it
        // sees needs to name the offending key.
        let err = RecordingSelector::from_json(&serde_json::json!({"arm": 1})).unwrap_err();
        assert!(err.contains("arm"), "{err}");
    }

    fn two_arms() -> Vec<BTreeMap<String, String>> {
        vec![
            labels(&[("source", "redis"), ("host", "web-01")]),
            labels(&[("source", "valkey"), ("host", "web-01")]),
        ]
    }

    #[test]
    fn resolves_to_the_one_matching_recording() {
        let s = RecordingSelector::parse(["source=valkey".to_string()]).unwrap();
        assert_eq!(s.resolve(&two_arms()), Ok(1));
    }

    #[test]
    fn no_match_is_an_error_not_a_default() {
        let s = RecordingSelector::parse(["source=nope".to_string()]).unwrap();
        assert_eq!(s.resolve(&two_arms()), Err(SelectError::NoMatch));
    }

    #[test]
    fn resolve_against_no_candidates_is_no_match() {
        // Not a panic, not "vacuously ambiguous" — an archive that somehow
        // holds no recordings is just as much "no match" as one that holds
        // several none of which fit.
        let s = RecordingSelector::default();
        assert_eq!(s.resolve(&[]), Err(SelectError::NoMatch));
    }

    #[test]
    fn several_matches_is_an_error_not_the_first() {
        // Both arms share host=web-01. Picking the first would silently
        // analyze one arm of an A/B and present it as the answer.
        let s = RecordingSelector::parse(["host=web-01".to_string()]).unwrap();
        assert_eq!(
            s.resolve(&two_arms()),
            Err(SelectError::Ambiguous(vec![0, 1]))
        );
    }

    #[test]
    fn ambiguous_reports_only_the_indices_that_matched() {
        // Three recordings, exactly two ("arm=a") match, and they are not
        // contiguous from zero. `two_arms` alone can't catch a `resolve`
        // that returns `(0..candidates.len())` instead of the real hits,
        // since there every candidate matches every ambiguous test selector.
        // The caller uses these indices to list which recordings the
        // selector matched, so a wrong payload here would show the user
        // recordings their selector never named.
        let candidates = vec![
            labels(&[("arm", "a"), ("host", "1")]),
            labels(&[("arm", "b"), ("host", "2")]),
            labels(&[("arm", "a"), ("host", "3")]),
        ];
        let s = RecordingSelector::parse(["arm=a".to_string()]).unwrap();
        assert_eq!(
            s.resolve(&candidates),
            Err(SelectError::Ambiguous(vec![0, 2]))
        );
    }

    #[test]
    fn an_empty_selector_over_several_recordings_is_ambiguous() {
        // "No selector given" reaches the caller as ambiguity over all of
        // them, so there is one code path for "you must choose".
        let s = RecordingSelector::default();
        assert_eq!(
            s.resolve(&two_arms()),
            Err(SelectError::Ambiguous(vec![0, 1]))
        );
    }

    #[test]
    fn an_empty_selector_over_one_recording_resolves() {
        let s = RecordingSelector::default();
        assert_eq!(s.resolve(&two_arms()[..1]), Ok(0));
    }

    /// Parse the `--recording ...` lines a listing prints and confirm each
    /// one, fed straight back through `parse` and `resolve`, actually names
    /// the recording it was printed under.
    ///
    /// A round trip is the only check strong enough for this job: a `picker`
    /// that is always the first label in key order (for `two_arms`, that's
    /// `host=web-01` for BOTH recordings, since "host" sorts before
    /// "source") would still pass plain substring assertions — `out` would
    /// contain "source=redis", "source=valkey", and "--recording" no matter
    /// what `picker` actually was. Feeding it back through `resolve` is what
    /// catches a selector that looks plausible but doesn't work.
    fn assert_listing_round_trips(
        all: &[BTreeMap<String, String>],
        out: &str,
        expect: &[usize],
        syntax: SelectorSyntax,
    ) {
        let pickers: Vec<&str> = out
            .lines()
            .filter_map(|l| l.trim_start().strip_prefix("select with: "))
            .collect();
        assert_eq!(
            pickers.len(),
            expect.len(),
            "one selector line per rendered recording: {out}"
        );
        for (picker, &i) in pickers.iter().zip(expect) {
            let s = reparse(picker, syntax);
            assert_eq!(
                s.resolve(all),
                Ok(i),
                "the selector printed for recording {i} must resolve back to it: {picker:?}\n{out}"
            );
        }
    }

    /// Both front ends' listings must round-trip, not just the CLI's.
    fn assert_listing_round_trips_both(
        all: &[BTreeMap<String, String>],
        highlight: &[usize],
        expect: &[usize],
    ) {
        for syntax in [SelectorSyntax::Flags, SelectorSyntax::Json] {
            let out = describe_candidates(all, highlight, syntax);
            assert_listing_round_trips(all, &out, expect, syntax);
        }
    }

    #[test]
    fn the_candidate_listing_names_every_recording_and_its_selector() {
        let all = two_arms();
        let out = describe_candidates(&all, &[], SelectorSyntax::Flags);
        assert!(out.contains("source=redis"), "{out}");
        assert!(out.contains("source=valkey"), "{out}");
        assert_listing_round_trips(&all, &out, &[0, 1], SelectorSyntax::Flags);
        assert_listing_round_trips_both(&all, &[], &[0, 1]);
        // No indices: they invite `--recording 1`, which is not a selector.
        // Checked against a few plausible numbering styles, not just one.
        for pat in ["[1]", "[2]", "(1)", "(2)", "1.", "2.", "#1", "#2"] {
            assert!(!out.contains(pat), "found index-like text {pat:?}: {out}");
        }
    }

    #[test]
    fn describe_candidates_computes_uniqueness_against_the_full_archive_not_the_highlighted_subset()
    {
        // `arm=a` matches recordings 0 and 1 (Ambiguous([0, 1])). Recording 2
        // is not in the highlighted set, but it still carries `host=1`, the
        // same as recording 0 — so `host=1` must NOT be offered as recording
        // 0's selector, even though within {0, 1} alone it would look
        // unique.
        let all = vec![
            labels(&[("arm", "a"), ("host", "1")]),
            labels(&[("arm", "a"), ("host", "2")]),
            labels(&[("arm", "b"), ("host", "1")]),
        ];
        assert_eq!(
            RecordingSelector::parse(["arm=a".to_string()])
                .unwrap()
                .resolve(&all),
            Err(SelectError::Ambiguous(vec![0, 1]))
        );

        assert_listing_round_trips_both(&all, &[0, 1], &[0, 1]);
    }

    #[test]
    fn identical_labels_are_reported_as_unselectable_not_a_dead_end_selector() {
        // Two recordings that carry exactly the same labels: the recorder
        // permits this (warning only) and the reader treats it as legal, so
        // this is reachable, not hypothetical. Offering a `--recording` for
        // either one would be a dead end — pasting it resolves to
        // `Ambiguous` again, forever.
        let dup = vec![
            labels(&[("host", "web-01"), ("source", "redis")]),
            labels(&[("host", "web-01"), ("source", "redis")]),
        ];
        let out = describe_candidates(&dup, &[], SelectorSyntax::Flags);
        assert!(
            !out.contains("select with:"),
            "no selector can pick between identical label sets, so none should be offered: {out}"
        );
        assert!(
            out.contains("re-capture") && out.contains("source=NAME"),
            "must explain why, echoing the recorder's own warning: {out}"
        );
        // An archive assembled by `recording combine` holds recordings that
        // were never `--endpoint`s of one run, so "give each --endpoint its
        // own source=NAME" is advice its owner cannot act on. The label they
        // can set is `record --label` on the inputs, before combining.
        assert!(
            out.contains("--label") && out.contains("combine"),
            "must also give the fix for a combined archive, whose recordings \
             were never endpoints of one run: {out}"
        );
    }

    /// A recording whose labels are a SUBSET of another's is just as
    /// unselectable as one that duplicates them, and this is the shape the
    /// documented A/B workflow produces: `record --label arm=baseline` on one
    /// capture and no `--label` on the other, then `recording combine`. The
    /// arms come out as `{arm, host, source}` and `{host, source}`.
    ///
    /// Every selector that matches the smaller set also matches the larger
    /// one, so `--recording host=H --recording source=rezolus` — the whole-set
    /// fallback — resolves to `Ambiguous`. Offering it is a closed loop: the
    /// caller is told to "add labels until it names one" and has no labels
    /// left to add. Reachable from a single `record --endpoint a --endpoint b`
    /// run too, when one endpoint's `/systeminfo` fetch fails and only that
    /// arm loses `host`.
    #[test]
    fn a_recording_whose_labels_are_a_subset_of_anothers_is_unselectable() {
        let all = vec![
            labels(&[("arm", "baseline"), ("host", "h"), ("source", "rezolus")]),
            labels(&[("host", "h"), ("source", "rezolus")]),
        ];
        let out = describe_candidates(&all, &[], SelectorSyntax::Flags);
        // Only recording 0 gets a selector (`arm=baseline`); recording 1 gets
        // the unselectable note. The round trip is what proves it: before the
        // fix, recording 1 was offered `--recording host=h --recording
        // source=rezolus`, which resolves to `Ambiguous([0, 1])`.
        assert_listing_round_trips(&all, &out, &[0], SelectorSyntax::Flags);
        assert_listing_round_trips_both(&all, &[], &[0]);
        assert!(
            out.contains("cannot be selected by labels"),
            "the subset arm must be reported as unselectable: {out}"
        );
    }

    /// The same failure in its smallest form: `{a:1}` against `{a:1, b:2}`.
    #[test]
    fn a_superset_recording_makes_its_subset_peer_unselectable() {
        let all = vec![labels(&[("a", "1")]), labels(&[("a", "1"), ("b", "2")])];
        assert_listing_round_trips_both(&all, &[], &[1]);
    }
}
