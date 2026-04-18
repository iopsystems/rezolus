pub mod dashboard;
mod plot;
mod service_extension;

pub use metriken_query::Tsdb;
pub use plot::*;
pub use service_extension::{Kpi, ServiceExtension, TemplateRegistry};

/// Default percentile quantiles used for histogram summaries.
pub const DEFAULT_PERCENTILES: &[f64] = &[0.5, 0.9, 0.99, 0.999, 0.9999];
