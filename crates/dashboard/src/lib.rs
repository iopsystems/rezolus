pub mod dashboard;
mod data;
pub mod events;
mod plot;
mod service_extension;
pub mod sql;

pub use dashboard::service::{substitute_view, substitute_view_and_p};
pub use data::{DashboardData, EmptyDashboardData};
pub use events::{Event, Events};
pub use plot::*;
pub use service_extension::{CategoryExtension, Kpi, ServiceExtension, TemplateRegistry};

/// Default percentile quantiles used for histogram summaries.
pub const DEFAULT_PERCENTILES: &[f64] = &[0.5, 0.9, 0.99, 0.999, 0.9999];
