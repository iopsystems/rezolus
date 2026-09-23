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
// prefix is the whole rule. Object key order is insertion order and the
// server emits labels sorted, so an internal label sorts before every
// letter; a legend that took "the first label that is not __name__" would
// print it.
export const isInternalLabel = (key) => key.startsWith('__');

// The labels a person is meant to see, as entries, in the order given.
export const visibleLabels = (metric) =>
    Object.entries(metric || {}).filter(([k]) => !isInternalLabel(k) && !isStorageKey(k));

// A short series name: the value of the first visible label, or null.
export const firstVisibleLabelValue = (metric) => {
    const first = visibleLabels(metric)[0];
    return first ? String(first[1]) : null;
};
