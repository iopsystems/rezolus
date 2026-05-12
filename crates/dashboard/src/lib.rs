pub mod dashboard;
pub mod events;
mod plot;
mod service_extension;

pub use events::{Event, Events};
pub use metriken_query::Tsdb;
pub use plot::*;
pub use service_extension::{CategoryExtension, Kpi, ServiceExtension, TemplateRegistry};

/// Default percentile quantiles used for histogram summaries.
pub const DEFAULT_PERCENTILES: &[f64] = &[0.5, 0.9, 0.99, 0.999, 0.9999];
