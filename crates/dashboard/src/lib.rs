pub mod capture_alias;
pub mod combined;
pub mod dashboard;
pub mod display_wire;
pub mod events;
pub mod keys;
pub mod metric_catalog;
mod plot;
pub mod rezolus_template;
pub mod section_template;
#[cfg(test)]
mod section_template_tests;
mod service_extension;
pub mod source_kind;
pub mod source_templates;

pub use events::{Event, Events};
// Re-export for callers that use &dyn MetricsSource through the dashboard crate.
pub use metriken_query::MetricsSource;
pub use plot::*;
pub use service_extension::{
    Bound, CategoryExtension, Check, Kpi, ServiceExtension, Severity, TemplateRegistry,
    format_duration, parse_duration,
};
#[cfg(not(target_arch = "wasm32"))]
pub use service_extension::{embedded_templates, resolve_templates};

/// Default percentile quantiles used for histogram summaries.
pub const DEFAULT_PERCENTILES: &[f64] = &[0.5, 0.9, 0.99, 0.999, 0.9999];
