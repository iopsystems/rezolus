// Compose the default download filename prefix for an A/B Report save.
// Caller appends the `.parquet.ab.tar` extension via the save modal.

const PARQUET_SUFFIXES = ['.parquet.ab.tar', '.parquet', '.ab.tar', '.tar'];

const stripKnownSuffix = (label) => {
    for (const suffix of PARQUET_SUFFIXES) {
        if (label.endsWith(suffix)) {
            return label.slice(0, -suffix.length);
        }
    }
    return label;
};

const sanitize = (raw, fallback) => {
    if (raw == null) return fallback;
    const basename = String(raw).split(/[/\\]/).pop();
    const stripped = stripKnownSuffix(basename).trim();
    if (!stripped) return fallback;
    const safe = stripped.replace(/[^A-Za-z0-9._-]+/g, '_').replace(/_+/g, '_');
    return safe || fallback;
};

export const composeAbReportPrefix = (baselineLabel, experimentLabel) => {
    const a = sanitize(baselineLabel, 'baseline');
    const b = sanitize(experimentLabel, 'experiment');
    return `${a}_vs_${b}`;
};
