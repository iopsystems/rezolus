use metriken::{Format, LazyCounter, MetricEntry};

use std::borrow::Cow;

/// Dynamic counters are used for metrics with scopes that are not known until
/// runtime. For example, having a counter for each CPU depends on runtime
/// discovery of how many CPUs. Other scopes might be network interfaces, block
/// devices, etc.
pub struct DynamicCounter {
    inner: metriken::DynBoxedMetric<LazyCounter>,
}

impl DynamicCounter {
    pub fn set(&self, value: u64) -> u64 {
        self.inner.set(value)
    }

    pub fn add(&self, value: u64) -> u64 {
        self.inner.add(value)
    }

    pub fn value(&self) -> u64 {
        self.inner.value()
    }
}

/// A builder for `DynamicCounter`s.
pub struct DynamicCounterBuilder {
    inner: metriken::MetricBuilder,
}

impl DynamicCounterBuilder {
    /// Create a new builder to construct a dynamic counter.
    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self {
            inner: metriken::MetricBuilder::new(name.into()),
        }
    }

    /// Consume the builder and return a `DynamicCounter`.
    pub fn build(self) -> DynamicCounter {
        let inner = self
            .inner
            .build(LazyCounter::new(metriken::Counter::default));

        DynamicCounter { inner }
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
