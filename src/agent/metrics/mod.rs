use metriken::{CounterGroup, GaugeGroup, WindowedCounterGroup, WindowedGaugeGroup};

/// Writing one slot's labels on a group metric.
///
/// An implementation detail of [`SlotIdentity`](crate::agent::identity::SlotIdentity),
/// which is the only thing that should call it. Identity that changes without
/// being published is invisible to a subscriber for the life of its
/// connection — there is no longer a per-tick diff to notice — so the way to
/// write it is the way that tells someone.
///
/// The trait is not gated; its metriken impls are. That is what lets a test
/// build a `SlotIdentity` over a fake on any platform, rather than the publish
/// path being exercised only where BPF runs.
pub trait GroupMetadata: Sync {
    /// Replace a slot's whole label set in one update.
    ///
    /// One call rather than one per label, which is what makes a slot's
    /// re-assignment atomic to a reader: setting four labels with four calls
    /// let a reader see a slot half-way through changing hands and attribute a
    /// new task's numbers under part of the old task's name.
    fn set_metadata(&self, idx: usize, labels: std::collections::BTreeMap<String, String>);
    fn clear_metadata(&self, idx: usize);
    /// Every populated slot and what it means, right now.
    ///
    /// What a consumer connecting mid-life needs: the broadcast only carries
    /// what changes AFTER it subscribes, so without this a slot that was
    /// assigned before it arrived and never moves again would never be
    /// described.
    fn metadata_snapshot(&self) -> Vec<(usize, std::collections::HashMap<String, String>)>;
}

impl GroupMetadata for CounterGroup {
    fn set_metadata(&self, idx: usize, labels: std::collections::BTreeMap<String, String>) {
        CounterGroup::set_metadata(self, idx, labels.into_iter().collect());
    }

    fn clear_metadata(&self, idx: usize) {
        CounterGroup::clear_metadata(self, idx);
    }

    fn metadata_snapshot(&self) -> Vec<(usize, std::collections::HashMap<String, String>)> {
        CounterGroup::metadata_snapshot(self)
    }
}

impl GroupMetadata for GaugeGroup {
    fn set_metadata(&self, idx: usize, labels: std::collections::BTreeMap<String, String>) {
        GaugeGroup::set_metadata(self, idx, labels.into_iter().collect());
    }

    fn clear_metadata(&self, idx: usize) {
        GaugeGroup::clear_metadata(self, idx);
    }

    fn metadata_snapshot(&self) -> Vec<(usize, std::collections::HashMap<String, String>)> {
        GaugeGroup::metadata_snapshot(self)
    }
}

impl GroupMetadata for WindowedCounterGroup {
    fn set_metadata(&self, idx: usize, labels: std::collections::BTreeMap<String, String>) {
        WindowedCounterGroup::set_metadata(self, idx, labels.into_iter().collect());
    }

    fn clear_metadata(&self, idx: usize) {
        WindowedCounterGroup::clear_metadata(self, idx);
    }

    fn metadata_snapshot(&self) -> Vec<(usize, std::collections::HashMap<String, String>)> {
        WindowedCounterGroup::metadata_snapshot(self)
    }
}

impl GroupMetadata for WindowedGaugeGroup {
    fn set_metadata(&self, idx: usize, labels: std::collections::BTreeMap<String, String>) {
        WindowedGaugeGroup::set_metadata(self, idx, labels.into_iter().collect());
    }

    fn clear_metadata(&self, idx: usize) {
        WindowedGaugeGroup::clear_metadata(self, idx);
    }

    fn metadata_snapshot(&self) -> Vec<(usize, std::collections::HashMap<String, String>)> {
        WindowedGaugeGroup::metadata_snapshot(self)
    }
}
