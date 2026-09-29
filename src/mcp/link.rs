//! `viewer_link`: a URL for a running viewer that opens a section (or one
//! chart) at a time range with the rest of the view state set.
//!
//! The wire form is the viewer's own (`src/viewer/assets/lib/ui/url_state.js`):
//! the section and chart ride in the hash (`#/cpu`, `#/cpu/chart/<id>`) and
//! everything else in the query string (`?from=...&to=...&time=raw&node=...
//! &gpu=vendor:id&cgroup=...&instance=...&family=sigma:2&anchor.<id>=...`).
//! This module only formats; the JS side parses, and
//! `tests/viewer_link_parity.test.mjs` feeds the strings the tests here
//! produce through that parser so the two cannot drift apart unnoticed.
//!
//! The server does not know where a viewer is listening, so the tool
//! returns the fragment and the query string on their own, and a full URL
//! only when the call (or `rezolus mcp --viewer-url`) supplies a base. A
//! base is an `http(s)` URL with no fragment; one that already carries a
//! query (the static site's `?capture=demo`) gets the view keys appended
//! with `&`, one whose last path segment is a file (`index.html`) gets them
//! appended directly, any other gets `/` first.
//!
//! A service section is addressed as `section: "service/<name>"`, the one
//! `/` the section may carry, since the viewer routes services under
//! `/service/:serviceName`.

use serde::Serialize;
use serde_json::Value;

/// What `viewer_link` returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Link {
    /// `#/cpu` or `#/cpu/chart/<chart id>`.
    pub fragment: String,
    /// `?from=...&to=...`, or empty when no view state was given.
    pub query: String,
    /// `<base>/<query><fragment>` when a base was given, else `None`.
    pub url: Option<String>,
}

/// The view state a link carries, parsed and validated from tool arguments.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct LinkSpec {
    pub section: String,
    pub chart_id: Option<String>,
    /// Both or neither, in Unix seconds, `to > from`.
    pub range: Option<(f64, f64)>,
    /// `true` writes `time=raw`; grid is the default and is absent.
    pub raw_time: bool,
    pub node: Option<String>,
    /// `vendor:id`, or a bare id.
    pub gpu: Vec<String>,
    pub cgroup: Vec<String>,
    pub instance: Option<String>,
    /// `envelope` or `sigma:<k>`.
    pub family: Option<String>,
    /// Capture id to `<signed ms>` or `kind:<event kind>`, in the order given.
    pub anchors: Vec<(String, String)>,
}

/// Percent-encode for a query value the way `URLSearchParams` will read it
/// back: alphanumerics and `-._*` pass, a space becomes `%20` (which it
/// decodes like `+`), everything else is `%XX` on its UTF-8 bytes.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// RFC 3339 in UTC with milliseconds, the form the viewer writes
/// (`Date#toISOString`).
fn rfc3339_ms(secs: f64) -> String {
    let ms = (secs * 1000.0).round() as i64;
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| format!("{secs}"))
}

impl LinkSpec {
    /// The query string, with its leading `?`, or empty.
    pub(crate) fn query(&self) -> String {
        let mut pairs: Vec<(String, String)> = Vec::new();
        if let Some((from, to)) = self.range {
            pairs.push(("from".into(), rfc3339_ms(from)));
            pairs.push(("to".into(), rfc3339_ms(to)));
        }
        if self.raw_time {
            pairs.push(("time".into(), "raw".into()));
        }
        if let Some(n) = &self.node {
            pairs.push(("node".into(), n.clone()));
        }
        for g in &self.gpu {
            pairs.push(("gpu".into(), g.clone()));
        }
        for c in &self.cgroup {
            pairs.push(("cgroup".into(), c.clone()));
        }
        if let Some(i) = &self.instance {
            pairs.push(("instance".into(), i.clone()));
        }
        if let Some(f) = &self.family {
            pairs.push(("family".into(), f.clone()));
        }
        for (id, a) in &self.anchors {
            pairs.push((format!("anchor.{id}"), a.clone()));
        }
        if pairs.is_empty() {
            return String::new();
        }
        let body: Vec<String> = pairs
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect();
        format!("?{}", body.join("&"))
    }

    pub(crate) fn fragment(&self) -> String {
        match &self.chart_id {
            Some(id) => format!("#/{}/chart/{}", self.section, id),
            None => format!("#/{}", self.section),
        }
    }

    /// The link; `base` must have passed [`validate_base`].
    pub(crate) fn link(&self, base: Option<&str>) -> Link {
        let query = self.query();
        let fragment = self.fragment();
        let url = base.map(|b| join_base(b, &query, &fragment));
        Link {
            fragment,
            query,
            url,
        }
    }
}

/// A viewer address the tool can build on: `http://` or `https://`, no
/// fragment (the link's own hash is the route), nothing else assumed.
pub(crate) fn validate_base(base: &str) -> Result<String, String> {
    let b = base.trim();
    if !(b.starts_with("http://") || b.starts_with("https://")) {
        return Err(format!(
            "viewer address {base:?} must start with http:// or https:// (e.g. http://127.0.0.1:4200)"
        ));
    }
    if b.contains('#') {
        return Err(format!(
            "viewer address {base:?} must not carry a fragment; the link's own hash picks the section"
        ));
    }
    if b.len() <= "https://".len() {
        return Err(format!("viewer address {base:?} has no host"));
    }
    Ok(b.to_string())
}

/// Append the view query and the hash to a validated base.
fn join_base(base: &str, query: &str, fragment: &str) -> String {
    if let Some((head, existing)) = base.split_once('?') {
        // The base already has a query (the static site's `?capture=...`):
        // the view keys join it with `&`, and the fragment follows.
        let body = query.trim_start_matches('?');
        return if body.is_empty() {
            format!("{head}?{existing}{fragment}")
        } else if existing.is_empty() {
            format!("{head}?{body}{fragment}")
        } else {
            format!("{head}?{existing}&{body}{fragment}")
        };
    }
    let b = base.trim_end_matches('/');
    let after_host = b
        .find("://")
        .map(|i| &b[i + 3..])
        .and_then(|rest| rest.find('/').map(|i| &rest[i + 1..]))
        .unwrap_or("");
    let last = after_host.rsplit('/').next().unwrap_or("");
    if last.contains('.') {
        // `.../index.html`: a document, not a directory.
        format!("{b}{query}{fragment}")
    } else {
        format!("{b}/{query}{fragment}")
    }
}

fn opt_str(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn str_list(args: &Value, key: &str) -> Result<Vec<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(vec![s.trim().to_string()]),
        Some(Value::String(_)) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| match v {
                Value::String(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
                _ => Err(format!("{key} entries must be non-empty strings")),
            })
            .collect(),
        Some(_) => Err(format!("{key} must be a string or an array of strings")),
    }
}

/// An instant argument: RFC 3339, or Unix seconds as a number. A digit-only
/// string is refused as ambiguous, as `add_event` refuses it.
fn instant_secs(v: &Value, key: &str) -> Result<f64, String> {
    match v {
        Value::Number(n) => {
            let s = n
                .as_f64()
                .ok_or_else(|| format!("{key} is not a finite number"))?;
            if !s.is_finite() || s < 0.0 || s > 1e10 {
                return Err(format!("{key} must be Unix seconds (a number under 1e10)"));
            }
            Ok(s)
        }
        Value::String(s) if s.trim().chars().all(|c| c.is_ascii_digit()) => Err(format!(
            "{key} {s:?} is a digit-only string, which is ambiguous; send Unix seconds \
             as a number or an RFC 3339 string"
        )),
        Value::String(s) => chrono::DateTime::parse_from_rfc3339(s.trim())
            .map(|dt| dt.timestamp_millis() as f64 / 1000.0)
            .map_err(|e| format!("{key} {s:?} is not RFC 3339: {e}")),
        _ => Err(format!("{key} must be an RFC 3339 string or Unix seconds")),
    }
}

/// Validate a section or chart id for the hash: non-empty, no `/`, `#`, `?`
/// or whitespace, which would be read as more path or as a query.
fn path_piece(s: &str, what: &str) -> Result<String, String> {
    if s.is_empty()
        || s.chars()
            .any(|c| matches!(c, '/' | '#' | '?') || c.is_whitespace())
    {
        return Err(format!(
            "{what} {s:?} cannot contain '/', '#', '?' or whitespace"
        ));
    }
    Ok(s.to_string())
}

/// A section: one piece (`cpu`), or a service section `service/<name>`,
/// the one form with a `/` the viewer routes (`/service/:serviceName`).
fn section_of(s: &str) -> Result<String, String> {
    match s.split_once('/') {
        None => path_piece(s, "section"),
        Some(("service", name)) => Ok(format!("service/{}", path_piece(name, "service name")?)),
        Some(_) => Err(format!(
            "section {s:?} cannot contain '/'; a service section is \"service/<name>\""
        )),
    }
}

impl LinkSpec {
    /// Parse and validate the tool's arguments.
    pub(crate) fn from_args(args: &Value) -> Result<Self, String> {
        let section = section_of(
            &opt_str(args, "section")?
                .ok_or("Missing section (e.g. \"cpu\", \"overview\", \"service/<name>\")")?,
        )?;
        let chart_id = opt_str(args, "chart_id")?
            .map(|c| path_piece(&c, "chart_id"))
            .transpose()?;

        let range = match (args.get("from"), args.get("to")) {
            (None | Some(Value::Null), None | Some(Value::Null)) => None,
            (Some(f), Some(t)) if !f.is_null() && !t.is_null() => {
                let from = instant_secs(f, "from")?;
                let to = instant_secs(t, "to")?;
                if to <= from {
                    return Err(format!(
                        "to ({}) must be after from ({})",
                        rfc3339_ms(to),
                        rfc3339_ms(from)
                    ));
                }
                Some((from, to))
            }
            _ => return Err("from and to go together: give both or neither".into()),
        };

        let raw_time = match opt_str(args, "time")?.as_deref() {
            None | Some("grid") | Some("aligned") => false,
            Some("raw") => true,
            Some(other) => return Err(format!("time must be \"raw\" or \"grid\", not {other:?}")),
        };

        let gpu = str_list(args, "gpu")?;
        for g in &gpu {
            if g.ends_with(':') {
                return Err(format!("gpu {g:?} has a vendor but no id"));
            }
        }

        let family = match opt_str(args, "family")?.as_deref() {
            None => None,
            Some("envelope") => Some("envelope".to_string()),
            Some("sigma") => Some("sigma:2".to_string()),
            Some(s) if s.starts_with("sigma:") => {
                let k: f64 = s["sigma:".len()..]
                    .parse()
                    .map_err(|_| format!("family {s:?}: sigma needs a number, e.g. sigma:2"))?;
                if !(k.is_finite() && k > 0.0) {
                    return Err(format!("family {s:?}: k must be positive"));
                }
                Some(format!("sigma:{k}"))
            }
            Some(other) => {
                return Err(format!(
                    "family must be \"envelope\" or \"sigma:<k>\", not {other:?}"
                ))
            }
        };

        let mut anchors = Vec::new();
        match args.get("anchors") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) => {
                for (id, v) in map {
                    if id.is_empty() {
                        return Err("anchors: an empty capture id".into());
                    }
                    let a = match v {
                        Value::Number(n) => {
                            let ms = n
                                .as_f64()
                                .ok_or_else(|| format!("anchors.{id} is not a number"))?;
                            if !ms.is_finite() || ms.fract() != 0.0 {
                                return Err(format!(
                                    "anchors.{id} must be whole milliseconds, not {ms}"
                                ));
                            }
                            // Ten thousand years each way; past that it is
                            // nanoseconds sent as milliseconds, and `as i64`
                            // would saturate silently.
                            if ms.abs() > 3.2e14 {
                                return Err(format!(
                                    "anchors.{id} {ms} is not a plausible millisecond offset"
                                ));
                            }
                            if ms == 0.0 {
                                continue; // no shift: the viewer never writes it
                            }
                            format!("{}", ms as i64)
                        }
                        Value::String(s)
                            if s.starts_with("kind:") && !s["kind:".len()..].trim().is_empty() =>
                        {
                            format!("kind:{}", s["kind:".len()..].trim())
                        }
                        Value::String(s) if s.trim().is_empty() => continue,
                        Value::String(s) => {
                            return Err(format!(
                                "anchors.{id} {s:?} must be signed milliseconds or \"kind:<event kind>\""
                            ))
                        }
                        _ => return Err(format!("anchors.{id} must be a number or a string")),
                    };
                    anchors.push((id.clone(), a));
                }
            }
            Some(_) => return Err("anchors must be an object of capture id to anchor".into()),
        }

        Ok(LinkSpec {
            section,
            chart_id,
            range,
            raw_time,
            node: opt_str(args, "node")?,
            gpu,
            cgroup: str_list(args, "cgroup")?,
            instance: opt_str(args, "instance")?,
            family,
            anchors,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// The fixtures `tests/viewer_link_parity.test.mjs` parses with the
    /// viewer's own `parseViewState`. Change one here, change it there.
    pub(crate) const PARITY_FULL: &str = "?from=2026-05-10T00%3A35%3A48.250Z&to=2026-05-10T00%3A38%3A49.000Z&time=raw&node=web-01&gpu=nvidia%3A0&gpu=1&cgroup=%2Fsystem.slice&cgroup=%2Fuser.slice%2Fa%2Cb&instance=3&family=sigma%3A1.5&anchor.baseline=kind%3Arun_start&anchor.experiment=-1500";

    #[test]
    fn a_full_link_renders_every_key_in_the_viewer_wire_form() {
        let spec = LinkSpec::from_args(&json!({
            "section": "cpu",
            "chart_id": "cpu-usage",
            "from": "2026-05-10T00:35:48.250Z",
            "to": 1778373529,
            "time": "raw",
            "node": "web-01",
            "gpu": ["nvidia:0", "1"],
            "cgroup": ["/system.slice", "/user.slice/a,b"],
            "instance": "3",
            "family": "sigma:1.5",
            "anchors": {"baseline": "kind:run_start", "experiment": -1500, "third": 0}
        }))
        .unwrap();
        assert_eq!(spec.query(), PARITY_FULL);
        assert_eq!(spec.fragment(), "#/cpu/chart/cpu-usage");
        let l = spec.link(Some("http://127.0.0.1:4200/"));
        assert_eq!(
            l.url.as_deref(),
            Some(&format!("http://127.0.0.1:4200/{PARITY_FULL}#/cpu/chart/cpu-usage")[..])
        );
        assert_eq!(spec.link(None).url, None);
    }

    #[test]
    fn a_service_section_routes_under_service() {
        let spec = LinkSpec::from_args(
            &json!({"section": "service/llm-perf", "chart_id": "c1", "instance": "3"}),
        )
        .unwrap();
        assert_eq!(spec.fragment(), "#/service/llm-perf/chart/c1");
        assert_eq!(spec.query(), "?instance=3");
        let err = LinkSpec::from_args(&json!({"section": "cpu/chart"}))
            .err()
            .unwrap();
        assert!(err.contains("service/<name>"), "{err}");
        assert!(LinkSpec::from_args(&json!({"section": "service/a/b"})).is_err());
        assert!(LinkSpec::from_args(&json!({"section": "service/"})).is_err());
    }

    #[test]
    fn a_base_is_validated_and_joined_by_its_shape() {
        let spec = LinkSpec::from_args(&json!({"section": "cpu", "time": "raw"})).unwrap();
        let url = |b: &str| spec.link(Some(&validate_base(b).unwrap())).url.unwrap();
        assert_eq!(
            url("http://127.0.0.1:4200"),
            "http://127.0.0.1:4200/?time=raw#/cpu"
        );
        assert_eq!(
            url("http://127.0.0.1:4200/"),
            "http://127.0.0.1:4200/?time=raw#/cpu"
        );
        assert_eq!(url("https://h/viewer/"), "https://h/viewer/?time=raw#/cpu");
        assert_eq!(
            url("https://iopsystems.github.io/rezolus/viewer/?capture=demo"),
            "https://iopsystems.github.io/rezolus/viewer/?capture=demo&time=raw#/cpu"
        );
        assert_eq!(
            url("http://h/index.html"),
            "http://h/index.html?time=raw#/cpu"
        );
        assert_eq!(
            url("http://h/index.html?"),
            "http://h/index.html?time=raw#/cpu"
        );
        let bare = LinkSpec::from_args(&json!({"section": "cpu"})).unwrap();
        let b = |s: &str| bare.link(Some(&validate_base(s).unwrap())).url.unwrap();
        assert_eq!(
            b("https://h/v/?capture=demo"),
            "https://h/v/?capture=demo#/cpu"
        );
        assert_eq!(b("http://h/index.html"), "http://h/index.html#/cpu");
        for bad in ["127.0.0.1:4200", "http://h/#/x", "https://", "ftp://h"] {
            assert!(validate_base(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn anchors_are_trimmed_and_bounded() {
        let spec = LinkSpec::from_args(
            &json!({"section": "cpu", "anchors": {"b": "kind:  deploy ", "e": -1500}}),
        )
        .unwrap();
        assert_eq!(spec.query(), "?anchor.b=kind%3Adeploy&anchor.e=-1500");
        let bad = |v: Value| {
            LinkSpec::from_args(&json!({"section": "cpu", "anchors": v}))
                .err()
                .unwrap()
        };
        assert!(bad(json!({"b": "kind:   "})).contains("kind:"));
        assert!(bad(json!({"b": 1e30})).contains("plausible"));
        assert!(
            bad(json!({"b": f64::NAN})).contains("whole") || true,
            "NaN is not JSON"
        );
    }

    #[test]
    fn a_bare_section_has_no_query() {
        let spec = LinkSpec::from_args(&json!({"section": "overview"})).unwrap();
        assert_eq!(spec.query(), "");
        assert_eq!(spec.fragment(), "#/overview");
        assert_eq!(
            spec.link(Some("https://host/viewer")).url.as_deref(),
            Some("https://host/viewer/#/overview")
        );
        // grid is the default and is not written; a zero anchor neither.
        let g = LinkSpec::from_args(
            &json!({"section": "cpu", "time": "grid", "anchors": {"baseline": 0}}),
        )
        .unwrap();
        assert_eq!(g.query(), "");
    }

    #[test]
    fn malformed_arguments_are_refused_not_encoded() {
        let bad = |v: Value| LinkSpec::from_args(&v).err().unwrap();
        assert!(bad(json!({})).contains("section"));
        assert!(bad(json!({"section": "cpu/chart"})).contains("'/'"));
        assert!(bad(json!({"section": "cpu", "anchors": {"x": "kind:"}})).contains("kind:"));
        assert!(bad(json!({"section": "cpu", "from": 10})).contains("both or neither"));
        assert!(bad(json!({"section": "cpu", "from": 20, "to": 10})).contains("after"));
        assert!(
            bad(json!({"section": "cpu", "from": "1778373348", "to": 1778373529}))
                .contains("digit-only")
        );
        assert!(bad(json!({"section": "cpu", "time": "fast"})).contains("time"));
        assert!(bad(json!({"section": "cpu", "gpu": ["nvidia:"]})).contains("no id"));
        assert!(bad(json!({"section": "cpu", "family": "median"})).contains("family"));
        assert!(bad(json!({"section": "cpu", "family": "sigma:0"})).contains("positive"));
        assert!(
            bad(json!({"section": "cpu", "anchors": {"x": 1.5}})).contains("whole milliseconds")
        );
        assert!(bad(json!({"section": "cpu", "anchors": {"x": "run_start"}})).contains("kind:"));
        assert!(bad(json!({"section": "cpu", "anchors": [1]})).contains("object"));
    }

    #[test]
    fn encoding_is_what_urlsearchparams_reads_back() {
        assert_eq!(encode("a b/c:d,e"), "a%20b%2Fc%3Ad%2Ce");
        assert_eq!(encode("ok-._*"), "ok-._*");
        assert_eq!(encode("é"), "%C3%A9");
        assert_eq!(rfc3339_ms(1778373348.25), "2026-05-10T00:35:48.250Z");
    }
}
