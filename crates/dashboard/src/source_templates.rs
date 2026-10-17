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
//!   `rezolus recording annotate --queries` writes and `rezolus recording
//!   check` reads. A combined parquet nests it under
//!   `per_source_metadata.<source>.<entry>.service_queries` instead.
//! - `dashboard`: reserved for a section template (schema 1), a document
//!   separate from `ServiceExtension`. It is read and kept in
//!   [`SourceTemplates::dashboard`] verbatim, and nothing consumes it yet: the
//!   section-template expander will, and it decides the format. The registry
//!   is consulted for each key independently.
//!
//! Precedence: the template in the metadata wins over the registry's entry
//! for the same source ([`TemplatePreference::Archive`], the default).
//! [`TemplatePreference::Registry`] inverts that for sources the registry
//! knows; a source the registry does not know keeps its own template either
//! way, since there is nothing to prefer over it. A `service_queries` value
//! that does not parse is not fatal: the source falls back to the registry and
//! the reason is returned in [`SourceTemplates::notices`].

use crate::{ServiceExtension, TemplateRegistry};
use std::collections::HashMap;

// Format keys owned by the binary's `src/parquet_metadata.rs`, duplicated as
// literals for the same reason as in `source_kind.rs`: this crate cannot
// depend on the binary.
const KEY_SOURCE: &str = "source";
const KEY_PER_SOURCE_METADATA: &str = "per_source_metadata";
/// The KPI template key, top level or nested under `per_source_metadata`.
pub const KEY_SERVICE_QUERIES: &str = "service_queries";
/// Reserved for a section template; see the module docs.
pub const KEY_DASHBOARD: &str = "dashboard";

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
    /// The raw `dashboard` value when the metadata carries one. Reserved:
    /// kept so the section-template expander can consume it, passed nowhere
    /// yet.
    pub dashboard: Option<String>,
    /// What the viewer should say about the lookup, such as a template in
    /// the metadata that did not parse.
    pub notices: Vec<String>,
}

/// Resolve one source's templates from its metadata.
///
/// `fallback_source` names the source when the metadata has no `source` key
/// (the WASM viewer passes `MetricsSource::source()`).
pub fn resolve_source_templates(
    metadata: &HashMap<String, String>,
    fallback_source: Option<&str>,
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
        .or(fallback_source)
        .filter(|s| !s.is_empty());

    // Picks between a template the metadata carried and the registry's.
    let choose = |source: &str, carried: ServiceExtension| -> ServiceExtension {
        match (preference, registry.get(source)) {
            (TemplatePreference::Registry, Some(ext)) => ext.clone(),
            _ => carried,
        }
    };

    // 1. Top-level `service_queries` (a single-source parquet, a `.rez`
    //    recording, a dendro source).
    if let Some(json) = metadata.get(KEY_SERVICE_QUERIES) {
        match serde_json::from_str::<ServiceExtension>(json) {
            Ok(ext) => {
                let source = top_source.unwrap_or(&ext.service_name).to_string();
                let ext = choose(&source, ext);
                out.service_exts.push((source, ext));
            }
            Err(e) => out.notices.push(malformed_notice(
                top_source.unwrap_or("(unnamed source)"),
                &e,
                registry,
                top_source,
            )),
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
            for entry in group.values() {
                let Some(sq) = entry.get(KEY_SERVICE_QUERIES) else {
                    continue;
                };
                match serde_json::from_value::<ServiceExtension>(sq.clone()) {
                    Ok(ext) => {
                        let ext = choose(source, ext);
                        out.service_exts.push((source.clone(), ext));
                        break; // one extension per source
                    }
                    Err(e) => out.notices.push(malformed_notice(
                        source,
                        &e,
                        registry,
                        Some(source.as_str()),
                    )),
                }
            }
        }

        // 3a. Sources without a template of their own: the registry.
        for source in map.keys() {
            if out.service_exts.iter().any(|(s, _)| s == source) {
                continue;
            }
            if let Some(ext) = registry.get(source) {
                out.service_exts.push((source.clone(), ext.clone()));
            }
        }
    }

    // 3b. No per-source map matched: the registry by the source's name.
    if out.service_exts.is_empty()
        && let Some(source) = top_source
        && let Some(ext) = registry.get(source)
    {
        out.service_exts.push((source.to_string(), ext.clone()));
    }

    out
}

fn malformed_notice(
    source: &str,
    err: &serde_json::Error,
    registry: &TemplateRegistry,
    lookup: Option<&str>,
) -> String {
    let fallback = if lookup.is_some_and(|s| registry.get(s).is_some()) {
        "using the built-in template instead"
    } else {
        "no built-in template matches, so it has no KPI sections"
    };
    format!(
        "source {source}: the service_queries template in its metadata does not parse ({err}); {fallback}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let t = resolve_source_templates(&m, None, &registry(), pref);
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
        let archive = resolve_source_templates(&m, None, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&archive), [("known".into(), "carried".into())]);
        let reg = resolve_source_templates(&m, None, &registry(), TemplatePreference::Registry);
        assert_eq!(titles(&reg), [("known".into(), "registry".into())]);
    }

    #[test]
    fn a_malformed_template_falls_back_to_the_registry_with_a_notice() {
        let m = md(&[
            ("source", "known".into()),
            ("service_queries", r#"{"service_name": 7}"#.into()),
        ]);
        let t = resolve_source_templates(&m, None, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&t), [("known".into(), "registry".into())]);
        assert_eq!(t.notices.len(), 1, "{:?}", t.notices);
        assert!(t.notices[0].contains("does not parse"), "{:?}", t.notices);
    }

    #[test]
    fn a_nested_template_in_a_combined_file_wins_over_the_registry() {
        let psm = serde_json::json!({
            "known": {"0": {"service_queries": ext("known", "carried")}},
        });
        let m = md(&[("per_source_metadata", psm.to_string())]);
        let t = resolve_source_templates(&m, None, &registry(), TemplatePreference::Archive);
        assert_eq!(titles(&t), [("known".into(), "carried".into())]);
        let t = resolve_source_templates(&m, None, &registry(), TemplatePreference::Registry);
        assert_eq!(titles(&t), [("known".into(), "registry".into())]);
    }

    #[test]
    fn the_fallback_name_finds_the_registry_entry() {
        let t = resolve_source_templates(
            &HashMap::new(),
            Some("known"),
            &registry(),
            TemplatePreference::Archive,
        );
        assert_eq!(titles(&t), [("known".into(), "registry".into())]);
    }

    #[test]
    fn the_dashboard_key_is_kept_verbatim() {
        let m = md(&[("dashboard", r#"{"schema":1}"#.into())]);
        let t = resolve_source_templates(&m, None, &registry(), TemplatePreference::Archive);
        assert_eq!(t.dashboard.as_deref(), Some(r#"{"schema":1}"#));
        assert!(t.service_exts.is_empty());
    }
}
