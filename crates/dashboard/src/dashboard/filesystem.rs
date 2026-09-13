//! One line per filesystem, named by its `mount` label.

use crate::MetricsSource;
use crate::plot::*;

fn by_mount(metric: &str) -> String {
    format!("sum by (mount) ({metric})")
}

pub fn generate(data: &dyn MetricsSource, sections: Vec<Section>) -> View {
    let mut view = View::new(data, sections);

    if !has_metric(data, "filesystem_total") {
        return view;
    }

    let mut space = Group::new("Space", "space");

    let capacity = space.subgroup("Capacity");
    capacity.describe(
        "Bytes per locally mounted filesystem. Available is what an unprivileged writer can \
         still use and is the number to alert on; Free also counts the superuser reserve. \
         Used % is computed as df computes it, so the reserve does not count as used.",
    );
    capacity.plot_promql(
        PlotOpts::gauge("Available", "available", Unit::Bytes),
        by_mount("filesystem_available"),
    );
    capacity.plot_promql(
        PlotOpts::gauge("Free", "free", Unit::Bytes),
        by_mount("filesystem_free"),
    );
    capacity.plot_promql(
        PlotOpts::gauge("Total", "total", Unit::Bytes),
        by_mount("filesystem_total"),
    );
    capacity.plot_promql(
        PlotOpts::gauge("Used %", "used-pct", Unit::Percentage).percentage_range(),
        // As df computes it: used over used plus available, so the superuser
        // reserve counts as neither.
        format!(
            "({total} - {free}) / ({total} - {free} + {available})",
            total = by_mount("filesystem_total"),
            free = by_mount("filesystem_free"),
            available = by_mount("filesystem_available"),
        ),
    );

    view.group(space);

    if has_metric(data, "filesystem_inodes_total") {
        let mut inodes = Group::new("Inodes", "inodes");

        let usage = inodes.subgroup("Usage");
        usage.describe(
            "Inodes per filesystem. A filesystem full of small files runs out of these before \
             it runs out of bytes. ext4 fixes the count at mkfs time; XFS and ZFS report an \
             estimate that moves with free space; btrfs and vfat report no limit and are not \
             shown.",
        );
        usage.plot_promql(
            PlotOpts::gauge("Free Inodes", "inodes-free", Unit::Count),
            by_mount("filesystem_inodes_free"),
        );
        usage.plot_promql(
            PlotOpts::gauge("Inodes Free %", "inodes-free-pct", Unit::Percentage)
                .percentage_range(),
            format!(
                "{} / {}",
                by_mount("filesystem_inodes_free"),
                by_mount("filesystem_inodes_total")
            ),
        );

        view.group(inodes);
    }

    if has_metric(data, "filesystem_readonly") {
        let mut state = Group::new("State", "state");

        let readonly = state.subgroup("Read-only");
        readonly.describe(
            "1 while a filesystem is read-only as a whole: mounted that way, or after an error \
             (ext4 emergency_ro, btrfs forced read-only). 0 does not prove it is writable: an XFS \
             shutdown does not show here, and a full filesystem stays writable while its writes \
             fail.",
        );
        readonly.plot_promql(
            PlotOpts::gauge("Read-only", "readonly", Unit::Count),
            by_mount("filesystem_readonly"),
        );

        view.group(state);
    }

    view
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriken_query::MemoryStore;
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    /// A store holding one sample of each named gauge, labeled `mount="/"`.
    fn store_with(metrics: &[&str]) -> MemoryStore {
        let store = MemoryStore::builder().sampling_interval_ms(1000).build();
        let gauges = metrics
            .iter()
            .map(|name| {
                let mut metadata = HashMap::new();
                metadata.insert("mount".to_string(), "/".to_string());
                metriken_exposition::Gauge::new(name.to_string(), 1, metadata)
            })
            .collect();
        store.ingest_snapshot(metriken_exposition::Snapshot::V2(
            metriken_exposition::SnapshotV2 {
                systemtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                duration: Duration::from_secs(0),
                metadata: HashMap::new(),
                counters: vec![],
                gauges,
                histograms: vec![],
            },
        ));
        store
    }

    fn json(view: &View) -> String {
        serde_json::to_string(view).unwrap().replace("\\\"", "\"")
    }

    #[test]
    fn one_line_per_filesystem_through_sum_by_mount() {
        let view = generate(
            &store_with(&[
                "filesystem_total",
                "filesystem_free",
                "filesystem_available",
            ]),
            vec![],
        );
        let j = json(&view);
        assert!(j.contains("sum by (mount) (filesystem_available)"));
        assert!(j.contains("sum by (mount) (filesystem_free)"));
        assert!(j.contains("sum by (mount) (filesystem_total)"));
        // Used % as df computes it, not 1 - available / total.
        assert!(j.contains(
            "(sum by (mount) (filesystem_total) - sum by (mount) (filesystem_free)) / \
             (sum by (mount) (filesystem_total) - sum by (mount) (filesystem_free) + \
             sum by (mount) (filesystem_available))"
        ));
    }

    #[test]
    fn inode_cards_appear_only_when_the_recording_has_inode_metrics() {
        let without = generate(&store_with(&["filesystem_total"]), vec![]);
        assert!(!json(&without).contains("filesystem_inodes"));

        let with = generate(
            &store_with(&[
                "filesystem_total",
                "filesystem_inodes_total",
                "filesystem_inodes_free",
            ]),
            vec![],
        );
        assert!(json(&with).contains(
            "sum by (mount) (filesystem_inodes_free) / sum by (mount) (filesystem_inodes_total)"
        ));
    }

    #[test]
    fn read_only_card_appears_only_when_the_recording_has_the_metric() {
        let without = generate(&store_with(&["filesystem_total"]), vec![]);
        assert!(!json(&without).contains("filesystem_readonly"));

        let with = generate(
            &store_with(&["filesystem_total", "filesystem_readonly"]),
            vec![],
        );
        assert!(json(&with).contains("sum by (mount) (filesystem_readonly)"));
    }

    #[test]
    fn a_recording_without_the_sampler_renders_an_empty_section() {
        let view = generate(&MemoryStore::builder().build(), vec![]);
        assert!(!json(&view).contains("filesystem_"));
    }
}
