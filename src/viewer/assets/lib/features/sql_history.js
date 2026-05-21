// Pure helpers for QueryExplorer's localStorage-backed history.
// Kept dependency-free so the unit tests can run under node without
// pulling in the browser-only chart stack.

export const HISTORY_KEY = 'sql_history';
export const LEGACY_HISTORY_KEY = 'promql_history';
export const MAX_HISTORY = 20;

// Trim entries to MAX_HISTORY and de-dupe by SQL string. Most-recent
// wins so re-running an old query bubbles it to the top.
export const trimAndDedupe = (entries) => {
    const seen = new Set();
    const out = [];
    for (const e of entries) {
        if (!e || !e.sql) continue;
        const key = e.sql.trim();
        if (!key) continue;
        if (seen.has(key)) continue;
        seen.add(key);
        out.push(e);
        if (out.length >= MAX_HISTORY) break;
    }
    return out;
};

export const readHistory = (storage = (typeof window !== 'undefined' ? window.localStorage : null)) => {
    if (!storage) return [];
    try {
        const raw = storage.getItem(HISTORY_KEY);
        if (raw) {
            const parsed = JSON.parse(raw);
            return Array.isArray(parsed) ? parsed.slice(0, MAX_HISTORY) : [];
        }
        // First-read migration from the pre-purge PromQL key. Copy
        // verbatim — stale PromQL just errors on submit, the user
        // cleans up via the history dropdown.
        const legacy = storage.getItem(LEGACY_HISTORY_KEY);
        if (legacy) {
            const parsed = JSON.parse(legacy);
            if (Array.isArray(parsed)) {
                const migrated = parsed.slice(0, MAX_HISTORY);
                storage.setItem(HISTORY_KEY, JSON.stringify(migrated));
                return migrated;
            }
        }
    } catch (_) {
        // localStorage unavailable or corrupt — start clean.
    }
    return [];
};

export const writeHistory = (entries, storage = (typeof window !== 'undefined' ? window.localStorage : null)) => {
    const trimmed = trimAndDedupe(entries);
    if (!storage) return trimmed;
    try {
        storage.setItem(HISTORY_KEY, JSON.stringify(trimmed));
    } catch (_) {
        // Read-only storage — return the in-memory trim regardless.
    }
    return trimmed;
};

export const pushHistory = (sql, storage) => {
    const trimmed = sql.trim();
    if (!trimmed) return readHistory(storage);
    const existing = readHistory(storage).filter((e) => e.sql !== trimmed);
    const updated = [{ sql: trimmed, at: Date.now() }, ...existing];
    return writeHistory(updated, storage);
};
