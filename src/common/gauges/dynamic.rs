use metriken::{Format, LazyGauge, MetricEntry};

use std::borrow::Cow;

/// Dynamic counters are used for metrics with scopes that are not known until
/// runtime. For example, having a counter for each CPU depends on runtime
/// discovery of how many CPUs. Other scopes might be network interfaces, block
/// devices, etc.
pub struct DynamicGauge {
    inner: metriken::DynBoxedMetric<LazyGauge>,
}

impl DynamicGauge {
    pub fn set(&self, value: i64) -> i64 {
        self.inner.set(value)
    }

    pub fn value(&self) -> i64 {
        self.inner.value()
    }
}

/// A builder for `DynamicCounter`s.
pub struct DynamicGaugeBuilder {
    inner: metriken::MetricBuilder,
}

impl DynamicGaugeBuilder {
    /// Create a new builder to construct a dynamic gauge.
    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self {
            inner: metriken::MetricBuilder::new(name.into()),
        }
    }

    /// Consume the builder and return a `DynamicCounter`.
    pub fn build(self) -> DynamicGauge {
        let inner = self.inner.build(LazyGauge::new(metriken::Gauge::default));

        DynamicGauge { inner }
    }

    /// Add a description for this metric.
    pub fn description(mut self, desc: impl Into<Cow<'static, str>>) -> Self {
        self.inner = self.inner.description(desc);
        self
    }

    /// Add a key-value metadata pair for this metric.
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.inner = self.inner.metadata(key, value);
        self
    }

    /// Provide a metric formatter for converting this metric into string
    /// representation for a given output format.
    pub fn formatter(mut self, formatter: fn(_: &MetricEntry, _: Format) -> String) -> Self {
        self.inner = self.inner.formatter(formatter);
        self
    }
}
