//! Which dashboard templates a source renders with, shared by both viewer
//! backends (the axum server in `src/viewer/` and the WASM crate in
//! `crates/viewer/`).
//!
//! A source's templates travel in its own metadata: a parquet's footer
//! key/values, a `.rez` recording's manifest metadata, or a dendro source's
//! metadata, which is what `MetricsSource::file_metadata` returns for each.
//! The built-in [`TemplateRegistry`] is the fallback for a source whose
//! metadata carries no template (parquet and `.rez` recorded before templates
//! travelled, and producers that do not send one).
//!
//! Keys, in lookup order:
//!
//! - `service_queries`: a [`ServiceExtension`] (KPI sections), the key
//!   `rezolus recording annotate` writes (the `--queries` file, or the
//!   registry's template for the source) and `rezolus recording check` reads.
//!   A combined parquet nests it under
//!   `per_source_metadata.<source>.<entry>.service_queries`, and its `source`
//!   is a JSON array of its sources, so a top-level template on it is keyed
//!   by the template's own `service_name`.
//! - `dashboard`: a section template (schema 1,
//!   [`SectionTemplate`]) for the Rezolus built-in sections, which a Rezolus
//!   agent sends in its stream handshake and a stream recording keeps in its
//!   source metadata. [`resolve_dashboard_template`] picks between it and the
//!   viewer's embedded template ([`crate::rezolus_template`]).
//!
//! Precedence: the template in the metadata wins over the registry's entry
//! for the same source ([`TemplatePreference::Archive`], the default).
//! [`TemplatePreference::Registry`] inverts that for sources the registry
//! knows; a source the registry does not know keeps its own template either
//! way, since there is nothing to prefer over it. A `service_queries` value
//! that does not parse is not fatal: the source falls back to the registry and
//! the reason is returned in [`SourceTemplates::notices`].
//!
//! The same preference applies to `dashboard`: under
//! [`TemplatePreference::Registry`] the embedded template renders whatever the
//! metadata carries. Under [`TemplatePreference::Archive`] the metadata's
//! template renders when it is at most [`MAX_DASHBOARD_BYTES`] and
//! [`SectionTemplate::parse`] accepts it and it has a section the viewer can
//! show (see [`crate::dashboard::is_reserved_route`]); otherwise the embedded
//! template renders and [`DashboardTemplate::notice`] says why. A recording
//! without the key (a parquet, a `.rez`, anything scraped rather than
//! streamed, or an agent older than this change) renders the embedded
//! template, silently.

use crate::section_template::SectionTemplate;
use crate::{ServiceExtension, TemplateRegistry};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// The KPI template key, top level or nested under `per_source_metadata`;
/// the section template key (see the module docs).
pub use crate::keys::{KEY_DASHBOARD, KEY_SERVICE_QUERIES};
use crate::keys::{KEY_PER_SOURCE_METADATA, KEY_SOURCE};

/// The largest `dashboard` value a viewer parses. The Rezolus template is
/// about 106 KB; [`SectionTemplate::parse`] has no size limit of its own.
pub const MAX_DASHBOARD_BYTES: usize = 1 << 20;

/// Whose template renders when a source's metadata carries one and the
/// registry has one for the same source name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TemplatePreference {
    /// The template in the recording's metadata (the default).
    #[default]
    Archive,
    /// The registry's template (`rezolus view --registry-templates`).
    Registry,
}

/// The templates resolved for one source's metadata.
#[derive(Clone, Debug, Default)]
pub struct SourceTemplates {
    /// KPI templates keyed by the source name they render for.
    pub service_exts: Vec<(String, ServiceExtension)>,
    /// The raw `dashboard` value when the metadata carries one;
    /// [`resolve_dashboard_template`] is what reads it.
    pub dashboard: Option<String>,
    /// What the viewer should say about the lookup, such as a template in
    /// the metadata that did not parse.
    pub notices: Vec<String>,
}

/// Resolve one source's templates from its metadata.
pub fn resolve_source_templates(
    metadata: &HashMap<String, String>,
    registry: &TemplateRegistry,
    preference: TemplatePreference,
) -> SourceTemplates {
    let mut out = SourceTemplates {
        dashboard: metadata.get(KEY_DASHBOARD).cloned(),
        ..Default::default()
    };
    let top_source = metadata
        .get(KEY_SOURCE)
        .map(String::as_str)
        .filter(|s| is_one_source_name(s));

    // Picks between a template the metadata carried and the registry's;
    // true when the registry's was picked.
    let choose = |source: &str, carried: ServiceExtension| -> (ServiceExtension, bool) {
        match (preference, registry.get(source)) {
            (TemplatePreference::Registry, Some(ext)) => (ext.clone(), true),
            _ => (carried, false),
        }
    };
    // Sources whose rendered template is the registry's rather than one the
    // metadata carried, so a notice can say which one renders.
    let mut from_registry: HashSet<String> = HashSet::new();

    // Templates in the metadata that did not parse: the source they belong
    // to, when one is named, and the error. Turned into notices at the end,
    // once it is known what rendered instead.
    let mut malformed: Vec<(Option<String>, serde_json::Error)> = Vec::new();

    // 1. Top-level `service_queries` (a single-source parquet, a `.rez`
    //    recording, a dendro source, or a combined parquet annotated after
    //    combining). Keyed by `source`, or by the template's own
    //    `service_name` when `source` names several sources.
    if let Some(json) = metadata.get(KEY_SERVICE_QUERIES) {
        match serde_json::from_str::<ServiceExtension>(json) {
            Ok(ext) => {
                let source = top_source.unwrap_or(&ext.service_name).to_string();
                let (ext, registry_won) = choose(&source, ext);
                if registry_won {
                    from_registry.insert(source.clone());
                }
                out.service_exts.push((source, ext));
            }
            Err(e) => malformed.push((top_source.map(str::to_string), e)),
        }
    }

    // 2. Nested under `per_source_metadata` (combined parquet files).
    let per_source = metadata.get(KEY_PER_SOURCE_METADATA).and_then(|json| {
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json).ok()
    });
    if let Some(map) = &per_source {
        for (source, group) in map {
            if out.service_exts.iter().any(|(s, _)| s == source) {
                continue;
            }
            let Some(group) = group.as_object() else {
                continue;
            };
            let mut first_error = None;
            let mut parsed = false;
            for entry in group.values() {
                let Some(sq) = entry.get(KEY_SERVICE_QUERIES) else {
                    continue;
                };
                match serde_json::from_value::<ServiceExtension>(sq.clone()) {
                    Ok(ext) => {
                        let (ext, registry_won) = choose(source, ext);
                        if registry_won {
                            from_registry.insert(source.clone());
                        }
                        out.service_exts.push((source.clone(), ext));
                        parsed = true;
                        break; // one extension per source
                    }
                    Err(e) => {
                        first_error.get_or_insert(e);
                    }
                }
            }
            if let (false, Some(e)) = (parsed, first_error) {
                malformed.push((Some(source.clone()), e));
            }
        }

        // 3a. Sources without a template of their own: the registry.
        for source in map.keys() {
            if out.service_exts.iter().any(|(s, _)| s == source) {
                continue;
            }
            if let Some(ext) = registry.get(source) {
                from_registry.insert(source.clone());
                out.service_exts.push((source.clone(), ext.clone()));
            }
        }
    }

    // 3b. No per-source map matched: the registry by the source's name.
    if out.service_exts.is_empty()
        && let Some(source) = top_source
        && let Some(ext) = registry.get(source)
    {
        from_registry.insert(source.to_string());
        out.service_exts.push((source.to_string(), ext.clone()));
    }

    for (source, err) in malformed {
        // What renders for the source instead: the registry's template, one
        // carried elsewhere in the metadata, or nothing.
        let rendered = source.as_deref().map(|s| {
            (
                out.service_exts.iter().any(|(name, _)| name == s),
                from_registry.contains(s),
            )
        });
        out.notices.push(match (source, rendered) {
            (Some(s), Some((true, true))) => format!(
                "source {s}: the service_queries template in its metadata does not parse \
                 ({err}); using the built-in template instead"
            ),
            (Some(s), Some((true, false))) => format!(
                "source {s}: a service_queries template in its metadata does not parse \
                 ({err}); another template in its metadata renders instead"
            ),
            // The registry knows the source but was not consulted: the
            // registry fallback by `source` (step 3b) runs only when nothing
            // else rendered, and a per-source map named other sources.
            (Some(s), _) if registry.get(&s).is_some() => format!(
                "source {s}: the service_queries template in its metadata does not parse \
                 ({err}); it has no KPI sections"
            ),
            (Some(s), _) => format!(
                "source {s}: the service_queries template in its metadata does not parse \
                 ({err}); no built-in template matches, so it has no KPI sections"
            ),
            (None, _) => format!(
                "the top-level service_queries template does not parse ({err}) and names no \
                 single source; it is ignored"
            ),
        });
    }

    out
}

/// The KPI templates for one source's metadata map: a parquet footer's
/// key/values, a `.rez` recording's manifest metadata, or a dendro source's
/// metadata, as `MetricsSource::file_metadata` returns each. Both viewers
/// and `rezolus recording check` go through here, so a recording's embedded
/// KPIs resolve the same way wherever they are read.
///
/// Order: top-level `service_queries`, then
/// `per_source_metadata.<source>.<entry>.service_queries`, then the registry
/// by source name ([`resolve_source_templates`]). A template in the metadata
/// wins over the registry's unless `preference` is `Registry`. A template
/// that does not parse falls back to the registry. The second element is
/// what a reader should log: [`SourceTemplates::notices`], then one notice
/// per KPI whose `unit_system` the viewer does not know (it renders with its
/// default `count` scaling).
pub fn service_extensions_from_metadata(
    metadata: &HashMap<String, String>,
    registry: &TemplateRegistry,
    preference: TemplatePreference,
) -> (Vec<(String, ServiceExtension)>, Vec<String>) {
    let resolved = resolve_source_templates(metadata, registry, preference);
    let mut notices = resolved.notices;
    for (source, ext) in &resolved.service_exts {
        for e in ext.unknown_unit_systems() {
            notices.push(format!(
                "service template for {source:?}: {e}; rendering it with the default `count` scaling"
            ));
        }
    }
    (resolved.service_exts, notices)
}

/// Where the section template a source renders with came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DashboardOrigin {
    /// The `dashboard` value in the source's metadata.
    Recording,
    /// The viewer's embedded template ([`embedded_dashboard_template`]).
    Embedded,
}

/// The section template chosen for one source.
#[derive(Clone, Debug)]
pub struct DashboardTemplate {
    pub template: Arc<SectionTemplate>,
    pub origin: DashboardOrigin,
    /// Why the metadata's template was not used, when it was present and
    /// refused (too large, [`SectionTemplate::parse`] rejected it, or no
    /// section is left once [`is_reserved_route`] sections are dropped), or
    /// which of its sections are not shown.
    ///
    /// [`is_reserved_route`]: crate::dashboard::is_reserved_route
    pub notice: Option<String>,
}

/// The Rezolus template this build embeds ([`crate::rezolus_template::json`]),
/// parsed once per process.
pub fn embedded_dashboard_template() -> Arc<SectionTemplate> {
    static EMBEDDED: OnceLock<Arc<SectionTemplate>> = OnceLock::new();
    EMBEDDED
        .get_or_init(|| {
            Arc::new(
                SectionTemplate::parse(&crate::rezolus_template::json())
                    .expect("the embedded Rezolus template parses"),
            )
        })
        .clone()
}

/// Parsed `dashboard` documents, by their text. A viewer holds one or two
/// captures at a time, and the terminal UI and each attach rebuild the
/// section context; this keeps them from reparsing the same document.
/// Refusals are kept too, so a document that does not parse is not parsed
/// again on every rebuild.
#[derive(Default)]
struct ParseCache {
    entries: Mutex<Vec<(String, Parsed)>>,
}

/// A parsed document, or why it was refused.
type Parsed = Result<Arc<SectionTemplate>, String>;

impl ParseCache {
    /// How many distinct documents the cache keeps; the oldest goes first.
    const ENTRIES: usize = 4;

    /// The process's cache.
    fn global() -> &'static ParseCache {
        static CACHE: OnceLock<ParseCache> = OnceLock::new();
        CACHE.get_or_init(Default::default)
    }

    /// Parse `json`, or return the result of parsing the same text before.
    fn parse(&self, json: &str) -> Parsed {
        if let Some((_, parsed)) = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(text, _)| text == json)
        {
            return parsed.clone();
        }
        // Parsed outside the lock: two callers racing on a new document both
        // parse it, and both results are equal.
        let parsed = SectionTemplate::parse(json)
            .map(Arc::new)
            .map_err(|e| e.to_string());
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if entries.len() >= Self::ENTRIES {
            drop(entries.remove(0));
        }
        entries.push((json.to_string(), parsed.clone()));
        parsed
    }
}

/// Choose the section template for a source's built-in sections from its
/// metadata; see the module docs for the order.
pub fn resolve_dashboard_template(
    metadata: &HashMap<String, String>,
    preference: TemplatePreference,
) -> DashboardTemplate {
    let embedded = |notice: Option<String>| DashboardTemplate {
        template: embedded_dashboard_template(),
        origin: DashboardOrigin::Embedded,
        notice,
    };
    let Some(json) = metadata.get(KEY_DASHBOARD) else {
        return embedded(None);
    };
    if preference == TemplatePreference::Registry {
        return embedded(None);
    }
    if json.len() > MAX_DASHBOARD_BYTES {
        return embedded(Some(format!(
            "the dashboard template in the recording's metadata is {} bytes, over the \
             {MAX_DASHBOARD_BYTES}-byte limit; using the built-in template instead",
            json.len()
        )));
    }
    match ParseCache::global().parse(json) {
        Ok(template) => {
            let dropped: Vec<String> = template
                .sections()
                .into_iter()
                .map(|s| s.route)
                .filter(|r| crate::dashboard::is_reserved_route(r))
                .collect();
            let listed = template.sections().len() - dropped.len();
            if listed == 0 {
                return embedded(Some(format!(
                    "the dashboard template in the recording's metadata has no section the \
                     viewer can show{}; using the built-in template instead",
                    dropped_routes(&dropped)
                )));
            }
            DashboardTemplate {
                template,
                origin: DashboardOrigin::Recording,
                notice: (!dropped.is_empty()).then(|| {
                    format!(
                        "the dashboard template in the recording's metadata has sections the \
                         viewer does not show{}",
                        dropped_routes(&dropped)
                    )
                }),
            }
        }
        Err(e) => embedded(Some(format!(
            "the dashboard template in the recording's metadata does not parse ({e}); \
             using the built-in template instead"
        ))),
    }
}

/// ` (routes a, b: reserved for the viewer, or more than one segment)`, or
/// nothing when `routes` is empty.
fn dropped_routes(routes: &[String]) -> String {
    if routes.is_empty() {
        String::new()
    } else {
        format!(
            " (routes {}: reserved for the viewer, or more than one segment)",
            routes.join(", ")
        )
    }
}

/// Whether a `source` value names one source. A combined parquet stores the
/// list of its sources as a JSON array (`["llm-perf","rezolus"]`), which names
/// none of them on its own.
pub(crate) fn is_one_source_name(value: &str) -> bool {
    !value.is_empty()
        && !matches!(
            serde_json::from_str::<serde_json::Value>(value),
            Ok(serde_json::Value::Array(_))
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lookup returns the resolver's notices first, then one per KPI
    /// whose `unit_system` no viewer knows; the extension is still returned.
    #[test]
    fn service_extensions_carry_their_notices() {
        let metadata: HashMap<String, String> = [
            ("source", "svc"),
            (
                "service_queries",
                r#"{"service_name": "svc", "kpis": [{"role": "load", "title": "Depth",
                    "query": "q", "type": "gauge", "unit_system": "furlongs"}]}"#,
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let (exts, notices) = service_extensions_from_metadata(
            &metadata,
            &TemplateRegistry::empty(),
            TemplatePreference::Archive,
        );
        assert_eq!(exts.len(), 1);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(
            notices[0].starts_with("service template for \"svc\": "),
            "{notices:?}"
        );
        assert!(notices[0].contains("furlongs"), "{notices:?}");

        let broken: HashMap<String, String> = [("source", "svc"), ("service_queries", "{")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let (exts, notices) = service_extensions_from_metadata(
            &broken,
            &TemplateRegistry::empty(),
            TemplatePreference::Archive,
        );
        assert!(exts.is_empty());
        assert_eq!(
            notices,
            resolve_source_templates(
                &broken,
                &TemplateRegistry::empty(),
                TemplatePreference::Archive
            )
            .notices
        );
        assert_eq!(notices.len(), 1, "{notices:?}");
    }

    fn ext(name: &str, title: &str) -> ServiceExtension {
        serde_json::from_value(serde_json::json!({
            "service_name": name,
            "kpis": [{"role": "throughput", "title": title, "query": "up", "type": "gauge"}],
        }))
        .unwrap()
    }

    fn md(pairs: &[(&str, String)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn titles(t: &SourceTemplates) -> Vec<(String, String)> {
        t.service_exts
            .iter()
            .map(|(s, e)| (s.clone(), e.kpis[0].title.clone()))
            .collect()
    }

    fn registry() -> TemplateRegistry {
        TemplateRegistry::from_templates(vec![ext("known", "registry")])
    }

    #[test]
    fn a_source_the_registry_does_not_know_renders_its_own_template() {
        let m = md(&[
            ("source", "acme".into()),
            (
                "service_queries",
                serde_json::to_string(&ext("acme", "carried")).unwrap(),
            ),
        ]);
        for pref in [TemplatePreference::Archive, TemplatePreference::Registry] {
            let t = resolve_source_templates(&m, &registry(), pref);
            assert_eq!(titles(&t), [("acme".into(), "carried".into())], "{pref:?}");
            assert!(t.notices.is_empty());
        }
    }

    #[test]
    fn the_carried_template_wins_unless_the_registry_is_preferred() {
        let m = md(&[
            ("source", "known".into()),
            (
                "service_queries",
                serde_json::to_string(&ext("known", "carried")).unwrap(),
            ),
        ]);
        let archive = resolve_source_templates(&m, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&archive), [("known".into(), "carried".into())]);
        let reg = resolve_source_templates(&m, &registry(), TemplatePreference::Registry);
        assert_eq!(titles(&reg), [("known".into(), "registry".into())]);
    }

    #[test]
    fn a_malformed_template_falls_back_to_the_registry_with_a_notice() {
        let m = md(&[
            ("source", "known".into()),
            ("service_queries", r#"{"service_name": 7}"#.into()),
        ]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&t), [("known".into(), "registry".into())]);
        assert_eq!(t.notices.len(), 1, "{:?}", t.notices);
        assert!(
            t.notices[0].contains("using the built-in template instead"),
            "{:?}",
            t.notices
        );
    }

    #[test]
    fn a_nested_template_in_a_combined_file_wins_over_the_registry() {
        let psm = serde_json::json!({
            "known": {"0": {"service_queries": ext("known", "carried")}},
        });
        let m = md(&[("per_source_metadata", psm.to_string())]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&t), [("known".into(), "carried".into())]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Registry);
        assert_eq!(titles(&t), [("known".into(), "registry".into())]);
    }

    #[test]
    fn a_source_name_alone_finds_the_registry_entry() {
        let m = md(&[("source", "known".into())]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&t), [("known".into(), "registry".into())]);
    }

    /// A combined parquet's `source` is a JSON array of its sources. A
    /// template annotated onto it at the top level is keyed by its own
    /// `service_name`, so it replaces that source's registry entry rather
    /// than rendering a second section named after the array.
    #[test]
    fn a_top_level_template_on_a_combined_file_is_keyed_by_its_service_name() {
        let psm = serde_json::json!({
            "known": {"0": {"version": "1"}},
            "other": {"0": {"version": "1"}},
            "rezolus": {"0": {"version": "6"}},
        });
        let registry = TemplateRegistry::from_templates(vec![
            ext("known", "registry"),
            ext("other", "registry"),
        ]);
        let m = md(&[
            ("source", r#"["known","other","rezolus"]"#.into()),
            ("per_source_metadata", psm.to_string()),
            (
                "service_queries",
                serde_json::to_string(&ext("known", "carried")).unwrap(),
            ),
        ]);
        let t = resolve_source_templates(&m, &registry, TemplatePreference::Archive);
        assert_eq!(
            titles(&t),
            [
                ("known".into(), "carried".into()),
                ("other".into(), "registry".into()),
            ]
        );
        let t = resolve_source_templates(&m, &registry, TemplatePreference::Registry);
        assert_eq!(
            titles(&t),
            [
                ("known".into(), "registry".into()),
                ("other".into(), "registry".into()),
            ]
        );
    }

    /// A malformed top-level template on a file whose `source` names several
    /// sources says it is ignored, not that no built-in template matched.
    #[test]
    fn a_malformed_template_naming_no_single_source_says_it_is_ignored() {
        let psm = serde_json::json!({"known": {"0": {}}});
        let m = md(&[
            ("source", r#"["known","rezolus"]"#.into()),
            ("per_source_metadata", psm.to_string()),
            ("service_queries", r#"{"service_name": 7}"#.into()),
        ]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&t), [("known".into(), "registry".into())]);
        assert_eq!(t.notices.len(), 1, "{:?}", t.notices);
        assert!(t.notices[0].contains("it is ignored"), "{:?}", t.notices);
    }

    /// A malformed nested entry raises no notice when a later entry of the
    /// same source parsed and rendered.
    #[test]
    fn a_malformed_nested_entry_is_quiet_when_another_entry_parsed() {
        let psm = serde_json::json!({
            "known": {
                "0": {"service_queries": {"service_name": 7}},
                "1": {"service_queries": ext("known", "carried")},
            },
        });
        let m = md(&[("per_source_metadata", psm.to_string())]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&t), [("known".into(), "carried".into())]);
        assert!(t.notices.is_empty(), "{:?}", t.notices);
    }

    /// A malformed top-level template whose source has a nested template that
    /// parsed names the nested one as what renders, not the built-in one.
    #[test]
    fn a_malformed_template_beside_a_carried_one_says_the_carried_one_renders() {
        let psm = serde_json::json!({
            "known": {"0": {"service_queries": ext("known", "carried")}},
        });
        let m = md(&[
            ("source", "known".into()),
            ("per_source_metadata", psm.to_string()),
            ("service_queries", r#"{"service_name": 7}"#.into()),
        ]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&t), [("known".into(), "carried".into())]);
        assert_eq!(t.notices.len(), 1, "{:?}", t.notices);
        assert!(
            t.notices[0].contains("another template in its metadata renders instead"),
            "{:?}",
            t.notices
        );
    }

    /// The same fixture with the registry preferred: the registry's template
    /// renders over the carried one, and the notice says so.
    #[test]
    fn a_malformed_template_beside_a_carried_one_under_registry_names_the_built_in() {
        let psm = serde_json::json!({
            "known": {"0": {"service_queries": ext("known", "carried")}},
        });
        let m = md(&[
            ("source", "known".into()),
            ("per_source_metadata", psm.to_string()),
            ("service_queries", r#"{"service_name": 7}"#.into()),
        ]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Registry);
        assert_eq!(titles(&t), [("known".into(), "registry".into())]);
        assert_eq!(t.notices.len(), 1, "{:?}", t.notices);
        assert!(
            t.notices[0].contains("using the built-in template instead"),
            "{:?}",
            t.notices
        );
    }

    /// A malformed nested template on a combined file falls back to the
    /// registry's for that source (step 3a), and the notice says so.
    #[test]
    fn a_malformed_nested_template_names_the_built_in_that_renders() {
        let psm = serde_json::json!({
            "known": {"0": {"service_queries": {"service_name": 8}}},
        });
        let m = md(&[
            ("source", r#"["known","rezolus"]"#.into()),
            ("per_source_metadata", psm.to_string()),
        ]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&t), [("known".into(), "registry".into())]);
        assert_eq!(t.notices.len(), 1, "{:?}", t.notices);
        assert!(
            t.notices[0].contains("using the built-in template instead"),
            "{:?}",
            t.notices
        );
    }

    /// `source` names a source the registry knows, its top-level template is
    /// malformed, and the per-source map names only another source (a file
    /// whose `source` was overwritten after combining). The registry entry by
    /// `source` is not used, since the map's sources rendered, so the notice
    /// says the source has no KPI sections without claiming no built-in
    /// template matches.
    #[test]
    fn a_malformed_template_for_a_source_outside_the_per_source_map_has_no_sections() {
        let registry = TemplateRegistry::from_templates(vec![
            ext("known", "registry"),
            ext("other", "registry"),
        ]);
        let psm = serde_json::json!({"other": {"0": {"version": "1"}}});
        let m = md(&[
            ("source", "known".into()),
            ("per_source_metadata", psm.to_string()),
            ("service_queries", r#"{"service_name": 7}"#.into()),
        ]);
        let t = resolve_source_templates(&m, &registry, TemplatePreference::Archive);
        assert_eq!(titles(&t), [("other".into(), "registry".into())]);
        assert_eq!(t.notices.len(), 1, "{:?}", t.notices);
        assert!(
            t.notices[0].contains("it has no KPI sections"),
            "{:?}",
            t.notices
        );
        assert!(
            !t.notices[0].contains("no built-in template matches"),
            "{:?}",
            t.notices
        );
    }

    /// A source with a malformed template and no registry entry is told it
    /// has no KPI sections.
    #[test]
    fn a_malformed_template_with_no_registry_entry_says_so() {
        let m = md(&[
            ("source", "acme".into()),
            ("service_queries", r#"{"service_name": 7}"#.into()),
        ]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Archive);
        assert!(t.service_exts.is_empty());
        assert_eq!(t.notices.len(), 1, "{:?}", t.notices);
        assert!(
            t.notices[0].contains("no built-in template matches"),
            "{:?}",
            t.notices
        );
    }

    /// The Rezolus template with the CPU section renamed `name`.
    fn renamed_cpu(name: &str) -> String {
        let mut doc: serde_json::Value =
            serde_json::from_str(&crate::rezolus_template::json()).unwrap();
        let cpu = doc["sections"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|s| s["route"] == "/cpu")
            .unwrap();
        cpu["name"] = serde_json::Value::String(name.to_string());
        doc.to_string()
    }

    fn cpu_name(t: &DashboardTemplate) -> String {
        t.template
            .sections()
            .into_iter()
            .find(|s| s.route == "/cpu")
            .unwrap()
            .name
    }

    #[test]
    fn metadata_without_a_dashboard_renders_the_embedded_template_silently() {
        let t = resolve_dashboard_template(&md(&[]), TemplatePreference::Archive);
        assert_eq!(t.origin, DashboardOrigin::Embedded);
        assert!(t.notice.is_none());
        assert!(Arc::ptr_eq(&t.template, &embedded_dashboard_template()));
    }

    #[test]
    fn the_recordings_dashboard_renders_unless_the_registry_is_preferred() {
        let m = md(&[("dashboard", renamed_cpu("Processors"))]);
        let t = resolve_dashboard_template(&m, TemplatePreference::Archive);
        assert_eq!(t.origin, DashboardOrigin::Recording);
        assert_eq!(cpu_name(&t), "Processors");
        assert!(t.notice.is_none());

        let t = resolve_dashboard_template(&m, TemplatePreference::Registry);
        assert_eq!(t.origin, DashboardOrigin::Embedded);
        assert_eq!(cpu_name(&t), "CPU");
        assert!(t.notice.is_none());
    }

    #[test]
    fn a_dashboard_that_does_not_parse_falls_back_with_a_notice() {
        let m = md(&[("dashboard", r#"{"schema": 1, "sections": 7}"#.into())]);
        let t = resolve_dashboard_template(&m, TemplatePreference::Archive);
        assert_eq!(t.origin, DashboardOrigin::Embedded);
        let notice = t.notice.expect("a notice");
        assert!(notice.contains("does not parse"), "{notice}");
        assert!(notice.contains("built-in template"), "{notice}");
    }

    /// A document over the limit is refused before it is parsed, even one
    /// that would parse: the padding is whitespace.
    #[test]
    fn a_dashboard_over_the_size_limit_falls_back_with_a_notice() {
        let mut big = renamed_cpu("Processors");
        big.push_str(&" ".repeat(MAX_DASHBOARD_BYTES + 1 - big.len()));
        assert!(SectionTemplate::parse(&big).is_ok());
        let t = resolve_dashboard_template(&md(&[("dashboard", big)]), TemplatePreference::Archive);
        assert_eq!(t.origin, DashboardOrigin::Embedded);
        assert_eq!(cpu_name(&t), "CPU");
        let notice = t.notice.expect("a notice");
        assert!(notice.contains("over the"), "{notice}");

        // At the limit, it is read.
        let mut at = renamed_cpu("Processors");
        at.push_str(&" ".repeat(MAX_DASHBOARD_BYTES - at.len()));
        let t = resolve_dashboard_template(&md(&[("dashboard", at)]), TemplatePreference::Archive);
        assert_eq!(t.origin, DashboardOrigin::Recording);
    }

    /// A template whose every section is one the viewer does not show falls
    /// back to the embedded one, saying which routes; one with some such
    /// sections renders, saying which are not shown.
    #[test]
    fn a_dashboard_with_no_showable_section_falls_back_with_a_notice() {
        let doc = |routes: &[&str]| {
            let sections: Vec<serde_json::Value> = routes
                .iter()
                .map(|r| serde_json::json!({"name": r, "route": r, "groups": []}))
                .collect();
            serde_json::json!({"schema": 1, "sections": sections}).to_string()
        };
        let m = md(&[("dashboard", doc(&["/about", "/x/y"]))]);
        let t = resolve_dashboard_template(&m, TemplatePreference::Archive);
        assert_eq!(t.origin, DashboardOrigin::Embedded);
        let notice = t.notice.expect("a notice");
        assert!(
            notice.contains("no section the viewer can show"),
            "{notice}"
        );
        assert!(notice.contains("/about, /x/y"), "{notice}");

        let m = md(&[("dashboard", doc(&["/fine", "/metadata"]))]);
        let t = resolve_dashboard_template(&m, TemplatePreference::Archive);
        assert_eq!(t.origin, DashboardOrigin::Recording);
        let notice = t.notice.expect("a notice");
        assert!(
            notice.contains("does not show (routes /metadata"),
            "{notice}"
        );

        let m = md(&[("dashboard", doc(&["/fine"]))]);
        let t = resolve_dashboard_template(&m, TemplatePreference::Archive);
        assert_eq!(t.origin, DashboardOrigin::Recording);
        assert!(t.notice.is_none());
    }

    /// The same document is parsed once: the second lookup returns the
    /// template the first parsed, until `ENTRIES` other documents have
    /// pushed it out. A cache of the test's own, since other tests use the
    /// process's at the same time.
    #[test]
    fn a_dashboard_is_parsed_once() {
        let cache = ParseCache::default();
        let doc = renamed_cpu("parsed once");
        let a = cache.parse(&doc).unwrap();
        let b = cache.parse(&doc).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        for i in 0..ParseCache::ENTRIES {
            cache.parse(&renamed_cpu(&format!("other {i}"))).unwrap();
        }
        let c = cache.parse(&doc).unwrap();
        assert!(!Arc::ptr_eq(&a, &c), "the oldest entry was not evicted");
        // A refusal is kept as one.
        assert!(cache.parse("{}").is_err());
        assert!(cache.parse("{}").is_err());
    }

    #[test]
    fn the_dashboard_key_is_kept_verbatim() {
        let m = md(&[("dashboard", r#"{"schema":1}"#.into())]);
        let t = resolve_source_templates(&m, &registry(), TemplatePreference::Archive);
        assert_eq!(t.dashboard.as_deref(), Some(r#"{"schema":1}"#));
        assert!(t.service_exts.is_empty());
    }
}
