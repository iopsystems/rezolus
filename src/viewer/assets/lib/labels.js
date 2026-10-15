// Label conventions shared by every part of the frontend that names, lists
// or hides labels. The rules are Prometheus's and are documented in
// docs/labels.md; the Rust side applies the same ones through
// metriken_query::is_internal_label and is_storage_key.

// Keys in column metadata that describe the column rather than the series.
// A query result never carries them, but a listing built from raw metadata
// can, so consumers that read metadata directly filter them the same way.
export const STORAGE_KEYS = ['metric', 'metric_type', 'unit', 'grouping_power', 'max_value_power'];

export const isStorageKey = (key) => STORAGE_KEYS.includes(key);

// An internal label is part of a series' identity and matchable in a
// selector, and never shown: `__name__`, `__run__`, `__incarnation__`. The
// prefix is the whole rule. The server's label order is not fixed, so a
// legend that took "the first label that is not __name__" could print one.
export const isInternalLabel = (key) => key.startsWith('__');

// The labels a person is meant to see, as entries, in the order given.
export const visibleLabels = (metric) =>
    Object.entries(metric || {}).filter(([k]) => !isInternalLabel(k) && !isStorageKey(k));

// A short series name: the value of the first visible label, or null.
export const firstVisibleLabelValue = (metric) => {
    const first = visibleLabels(metric)[0];
    return first ? String(first[1]) : null;
};

// The visible label keys whose values differ between the series of one
// result, sorted. A key a series lacks counts as a value of its own. Labels
// every series shares, such as a recording's `host` and `source`, are not
// among them.
export const varyingLabelKeys = (metrics) => {
    const keys = new Set();
    for (const m of metrics) for (const [k] of visibleLabels(m)) keys.add(k);
    const missing = Symbol('missing');
    return [...keys]
        .filter((k) => new Set(metrics.map((m) => (m && k in m ? m[k] : missing))).size > 1)
        .sort();
};

// Names a series by `keys`: the value of the one key, or `key=value` pairs
// when there are several. A name is null when the series has none of them.
export const namerForKeys = (keys) => {
    if (keys.length === 1) {
        const [k] = keys;
        return (m) => (m && m[k] != null ? String(m[k]) : null);
    }
    return (m) => keys.filter((k) => m && m[k] != null).map((k) => `${k}=${m[k]}`).join(', ') || null;
};

// Names for the series of one result, by the labels that differ between
// them (`namerForKeys`). A result whose series differ in no visible label
// is named as a single series is, by its first visible label.
export const seriesNamer = (metrics) => {
    const keys = varyingLabelKeys(metrics);
    return keys.length === 0 ? firstVisibleLabelValue : namerForKeys(keys);
};

// Names for an experiment's series in compare mode: by the labels that
// differ between the baseline's series, so the two sides pair, when that
// gives every series its own name; otherwise by the experiment's own.
export const pairedSeriesNamer = (baselineMetrics, metrics) => {
    const own = seriesNamer(metrics);
    const keys = varyingLabelKeys(baselineMetrics);
    if (keys.length === 0) return own;
    const byBaseline = namerForKeys(keys);
    const names = metrics.map(byBaseline);
    return names.every((n) => n != null) && new Set(names).size === names.length ? byBaseline : own;
};
