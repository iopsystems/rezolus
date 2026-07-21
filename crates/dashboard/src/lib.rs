pub mod dashboard;
pub mod display_wire;
pub mod events;
pub mod metric_catalog;
mod plot;
mod service_extension;
pub mod source_kind;

pub use events::{Event, Events};
// Re-export for callers that use &dyn MetricsSource through the dashboard crate.
pub use metriken_query::MetricsSource;
pub use plot::*;
pub use service_extension::{CategoryExtension, Kpi, ServiceExtension, TemplateRegistry};

/// Default percentile quantiles used for histogram summaries.
pub const DEFAULT_PERCENTILES: &[f64] = &[0.5, 0.9, 0.99, 0.999, 0.9999];
