//! Hostname allowlist for the viewer's URL proxy.
//!
//! Patterns are shell-style with `*` matching a single DNS label. So
//! `*.s3.amazonaws.com` matches `bucket.s3.amazonaws.com` but NOT
//! `bucket.x.s3.amazonaws.com`. To allow multiple subdomain levels,
//! list multiple patterns (`*.s3.amazonaws.com`, `*.*.s3.amazonaws.com`).
//!
//! The match is case-insensitive (DNS is). An empty pattern list
//! matches nothing — proxy stays effectively disabled.

#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    /// `--proxy-allow-any` short-circuits — every host passes,
    /// regardless of `patterns`. Deliberate opt-in escape hatch for
    /// "I'm running this on my own laptop, just let me load anything."
    any: bool,
    patterns: Vec<Vec<Label>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Label {
    Wildcard,
    Literal(String),
}

impl Allowlist {
    pub fn new(patterns: impl IntoIterator<Item = String>) -> Self {
        let patterns = patterns
            .into_iter()
            .filter(|p| !p.is_empty())
            .map(|p| {
                p.to_ascii_lowercase()
                    .split('.')
                    .map(|label| {
                        if label == "*" {
                            Label::Wildcard
                        } else {
                            Label::Literal(label.to_string())
                        }
                    })
                    .collect()
            })
            .collect();
        Self {
            any: false,
            patterns,
        }
    }

    pub fn any() -> Self {
        Self {
            any: true,
            patterns: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.any && self.patterns.is_empty()
    }

    pub fn is_any(&self) -> bool {
        self.any
    }

    pub fn allows(&self, host: &str) -> bool {
        if self.any {
            return true;
        }
        let host = host.to_ascii_lowercase();
        let host_labels: Vec<&str> = host.split('.').collect();
        self.patterns.iter().any(|pattern| {
            pattern.len() == host_labels.len()
                && pattern
                    .iter()
                    .zip(host_labels.iter())
                    .all(|(p, h)| match p {
                        Label::Wildcard => true,
                        Label::Literal(s) => s == h,
                    })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(patterns: &[&str]) -> Allowlist {
        Allowlist::new(patterns.iter().map(|s| s.to_string()))
    }

    #[test]
    fn empty_allowlist_blocks_everything() {
        let a = allow(&[]);
        assert!(a.is_empty());
        assert!(!a.allows("anything.com"));
    }

    #[test]
    fn exact_host_matches_itself() {
        let a = allow(&["s3.amazonaws.com"]);
        assert!(a.allows("s3.amazonaws.com"));
    }

    #[test]
    fn exact_host_does_not_match_subdomain() {
        let a = allow(&["s3.amazonaws.com"]);
        assert!(!a.allows("bucket.s3.amazonaws.com"));
    }

    #[test]
    fn wildcard_matches_single_label_subdomain() {
        let a = allow(&["*.s3.amazonaws.com"]);
        assert!(a.allows("bucket.s3.amazonaws.com"));
    }

    #[test]
    fn wildcard_does_not_match_apex() {
        // `*.foo.com` requires a leading label. `foo.com` itself has
        // one fewer label than the pattern, so it must not match.
        let a = allow(&["*.s3.amazonaws.com"]);
        assert!(!a.allows("s3.amazonaws.com"));
    }

    #[test]
    fn single_wildcard_does_not_span_multiple_labels() {
        // Single `*` is one DNS label only — `*.foo.com` does not
        // match `bar.baz.foo.com`. Users who want multi-level should
        // list multiple patterns.
        let a = allow(&["*.s3.amazonaws.com"]);
        assert!(!a.allows("bucket.x.s3.amazonaws.com"));
    }

    #[test]
    fn case_insensitive_match() {
        // DNS is case-insensitive; the matcher must agree.
        let a = allow(&["*.S3.AmazonAWS.com"]);
        assert!(a.allows("Bucket.s3.amazonaws.COM"));
    }

    #[test]
    fn multiple_patterns_any_match_passes() {
        let a = allow(&["s3.amazonaws.com", "*.internal.example.com"]);
        assert!(a.allows("s3.amazonaws.com"));
        assert!(a.allows("svc.internal.example.com"));
        assert!(!a.allows("svc.external.example.com"));
    }

    #[test]
    fn empty_pattern_strings_are_dropped() {
        let a = Allowlist::new(["".to_string(), "s3.amazonaws.com".to_string()]);
        assert!(!a.is_empty());
        assert!(a.allows("s3.amazonaws.com"));
    }

    #[test]
    fn wildcard_in_middle_position() {
        let a = allow(&["bucket.*.example.com"]);
        assert!(a.allows("bucket.us-west.example.com"));
        assert!(!a.allows("bucket.example.com"));
        assert!(!a.allows("other.us-west.example.com"));
    }

    #[test]
    fn any_allows_every_host() {
        // --proxy-allow-any is the deliberate escape hatch. is_empty
        // must report false (proxy is enabled), and allows() must
        // accept arbitrary label counts and shapes.
        let a = Allowlist::any();
        assert!(!a.is_empty());
        assert!(a.allows("example.com"));
        assert!(a.allows("bucket.s3.amazonaws.com"));
        assert!(a.allows("a.b.c.d.e.f.g"));
        assert!(a.allows("localhost"));
    }
}
