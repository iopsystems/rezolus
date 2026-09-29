// View state in the URL query string, so a pasted link reproduces the view.
//
// The section and chart live in the hash (`#/cpu/chart/<id>`, mithril's
// router); everything else the viewer shows for that section rides in
// `location.search`:
//
//   ?from=2026-09-28T14:03:11.250Z&to=2026-09-28T14:05:40Z&time=raw
//    &node=web-01&gpu=nvidia:0&gpu=nvidia:1&cgroup=/system.slice
//    &instance=3&anchor.experiment=-1500
//
// The query string and not the hash because `m.route.get()` returns the hash
// path including any query on it, and several places parse that path by hand
// (`route.replace(/^\//, '')`, `split('/')`); a query on the hash would turn
// `cpu?from=` into a section key. mithril pushes fragment-only URLs
// (`history.pushState(..., '#' + path)`), which keep the document's search
// intact, so the query survives every route change and sidebar link.
//
// This module owns only VIEW_KEYS (and `anchor.*`). Every other parameter,
// `capture=`, `compare=`, `demo*`, `category=`, passes through untouched, in
// place, so the static site's own canonicalisation and this one compose.
//
// The core is pure (string in, string out) and node-testable; the three
// browser wrappers at the bottom no-op without a `window`, so modules that
// import this stay importable under `node --test`.

export const VIEW_KEYS = [
    'from',
    'to',
    'time',
    'node',
    'gpu',
    'cgroup',
    'instance',
    'family',
    'anchor.baseline',
    'anchor.experiment',
];

// `family=sigma:2` (mean ± 2·sd) or `family=envelope` (min..max): the
// baseline is every attached capture but the experiment, drawn as a band.
// Absent means a plain A/B.
export const parseFamily = (s) => {
    if (typeof s !== 'string' || s === '') return null;
    if (s === 'envelope') return { kind: 'envelope', k: 2 };
    const m = /^sigma(?::(\d+(?:\.\d+)?))?$/.exec(s);
    if (!m) return null;
    const k = m[1] != null ? Number(m[1]) : 2;
    return { kind: 'sigma', k: Number.isFinite(k) && k > 0 ? k : 2 };
};

export const formatFamily = (f) => {
    if (!f || typeof f !== 'object') return null;
    if (f.kind === 'envelope') return 'envelope';
    const k = Number.isFinite(f.k) && f.k > 0 ? f.k : 2;
    return `sigma:${k}`;
};

const ANCHOR_PREFIX = 'anchor.';
const KIND_PREFIX = 'kind:';

const isOwnedKey = (k) => VIEW_KEYS.includes(k) || k.startsWith(ANCHOR_PREFIX);

// `from`/`to`: RFC 3339 (what we write, `Date#toISOString`), or Unix seconds,
// fractional allowed. Nanoseconds are refused on purpose: an ns-since-epoch is
// ~1.7e18, past 2^53, and would not survive a JS Number.
const parseInstantSec = (s) => {
    if (typeof s !== 'string' || s.trim() === '') return null;
    // URLSearchParams decodes `+` as a space, so an RFC 3339 offset typed
    // into the address bar (`...T14:03:11+02:00`) arrives as `... 02:00`.
    // Put the sign back; a real space cannot occur there.
    const t = s.trim().replace(/ (\d{2}:\d{2})$/, '+$1');
    if (/^-?\d+(\.\d+)?$/.test(t)) {
        const n = Number(t);
        // Anything above 1e12 is ms or finer, not seconds.
        return Number.isFinite(n) && n > 0 && n < 1e12 ? n : null;
    }
    const ms = Date.parse(t);
    return Number.isFinite(ms) ? ms / 1000 : null;
};

const formatInstantSec = (sec) =>
    Number.isFinite(sec) ? new Date(Math.round(sec * 1000)).toISOString() : null;

// `raw` is the only value written; `grid` is the default and is absent. Read
// accepts the label alias too, since "Aligned" is what the control says.
const parseTimeMode = (s) => {
    if (s === 'raw') return 'raw';
    if (s === 'grid' || s === 'aligned') return 'grid';
    return null;
};

// `gpu=vendor:id`, or a bare id when the sampler set no vendor. Split at the
// FIRST colon so a vendor never swallows the id.
const parseGpu = (s) => {
    if (typeof s !== 'string' || s === '') return null;
    const i = s.indexOf(':');
    if (i < 0) return { vendor: null, id: s };
    const vendor = s.slice(0, i);
    const id = s.slice(i + 1);
    if (id === '') return null;
    return { vendor: vendor === '' ? null : vendor, id };
};

const formatGpu = (g) => {
    if (g == null) return null;
    if (typeof g !== 'object') return String(g);
    if (g.id == null || String(g.id) === '') return null;
    return g.vendor != null && String(g.vendor) !== '' ? `${g.vendor}:${g.id}` : String(g.id);
};

// An anchor is a signed integer ms offset, or `kind:<event kind>` (resolved
// per capture against that capture's events). `0` means "no shift" and is
// not written.
export const parseAnchor = (s) => {
    if (typeof s !== 'string' || s === '') return null;
    if (s.startsWith(KIND_PREFIX)) {
        const kind = s.slice(KIND_PREFIX.length).trim();
        return kind ? { kind } : null;
    }
    if (/^-?\d+$/.test(s)) return Number(s);
    return null;
};

export const formatAnchor = (v) => {
    if (v == null) return null;
    if (typeof v === 'object') {
        const kind = typeof v.kind === 'string' ? v.kind.trim() : '';
        return kind ? `${KIND_PREFIX}${kind}` : null;
    }
    const n = Number(v);
    if (!Number.isFinite(n) || Math.round(n) === 0) return null;
    return String(Math.round(n));
};

const toParams = (search) => new URLSearchParams(
    typeof search === 'string' ? search.replace(/^\?/, '') : '',
);

/**
 * Parse a query string into view state. Malformed values are dropped, never
 * thrown: a link with a bad `from` opens at the full range, not an error.
 *
 * Returns `{ from, to, time, node, cgroup, gpu, instance, anchors }` where
 * `from`/`to` are epoch seconds (both or neither, and `to > from`), `time`
 * is `'raw' | 'grid' | null`, `cgroup` and `gpu` are arrays, and `anchors`
 * maps capture id to a number or `{kind}`.
 */
export function parseViewState(search) {
    const p = toParams(search);
    let from = parseInstantSec(p.get('from'));
    let to = parseInstantSec(p.get('to'));
    if (from == null || to == null || !(to > from)) {
        from = null;
        to = null;
    }
    const node = p.get('node');
    const instance = p.get('instance');
    const anchors = {};
    for (const [k, v] of p.entries()) {
        if (!k.startsWith(ANCHOR_PREFIX)) continue;
        const id = k.slice(ANCHOR_PREFIX.length);
        const a = parseAnchor(v);
        if (id && a != null) anchors[id] = a;
    }
    return {
        from,
        to,
        time: parseTimeMode(p.get('time')),
        node: node ? node : null,
        cgroup: p.getAll('cgroup').filter((c) => c !== ''),
        gpu: p.getAll('gpu').map(parseGpu).filter(Boolean),
        instance: instance ? instance : null,
        family: parseFamily(p.get('family')),
        anchors,
    };
}

/**
 * Apply a partial view state to a query string and return the new one
 * (with its leading `?`, or `''` when empty). Only the keys named in `patch`
 * change; a `null`, empty, or all-default value deletes its key. Unknown
 * parameters are left exactly where they were. When nothing would change,
 * the input is returned as is, so callers can call this unconditionally.
 *
 * `patch` fields: `from`, `to` (epoch seconds), `time`, `node`, `gpu`
 * (array), `cgroup` (array), `instance`, `anchors` (id -> value|null).
 */
export function applyViewState(search, patch = {}) {
    const p = toParams(search);
    const before = p.toString();

    const setOrDelete = (k, v) => {
        p.delete(k);
        if (v != null && v !== '') p.set(k, v);
    };
    const setList = (k, vals) => {
        p.delete(k);
        for (const v of vals) if (v != null && v !== '') p.append(k, v);
    };

    if ('from' in patch || 'to' in patch) {
        const from = Number.isFinite(patch.from) ? patch.from : null;
        const to = Number.isFinite(patch.to) ? patch.to : null;
        // Compared at the precision written (ms), or two instants that
        // differ by less than a millisecond would be written equal and
        // rejected on read.
        const ok = from != null && to != null
            && Math.round(to * 1000) > Math.round(from * 1000);
        setOrDelete('from', ok ? formatInstantSec(from) : null);
        setOrDelete('to', ok ? formatInstantSec(to) : null);
    }
    if ('time' in patch) {
        setOrDelete('time', patch.time === 'raw' ? 'raw' : null);
    }
    if ('node' in patch) setOrDelete('node', patch.node);
    if ('family' in patch) setOrDelete('family', formatFamily(patch.family));
    if ('instance' in patch) setOrDelete('instance', patch.instance == null ? null : String(patch.instance));
    if ('gpu' in patch) {
        setList('gpu', (Array.isArray(patch.gpu) ? patch.gpu : []).map(formatGpu));
    }
    if ('cgroup' in patch) {
        setList('cgroup', Array.isArray(patch.cgroup) ? patch.cgroup.map(String) : []);
    }
    if (patch.anchors && typeof patch.anchors === 'object') {
        for (const [id, v] of Object.entries(patch.anchors)) {
            // An empty id is never written, but a stray `anchor.=` in a
            // link is still ours to remove.
            setOrDelete(`${ANCHOR_PREFIX}${id}`, id ? formatAnchor(v) : null);
        }
    }

    const after = p.toString();
    if (after === before) return typeof search === 'string' ? search : '';
    return after ? `?${after}` : '';
}

/** The patch that removes every key this module owns. */
export const CLEAR_PATCH = Object.freeze({
    from: null,
    to: null,
    time: null,
    node: null,
    gpu: [],
    cgroup: [],
    instance: null,
    family: null,
    anchors: { baseline: null, experiment: null },
});

/** Every owned key present in a query string, for a full clear. */
export function ownedKeysIn(search) {
    return [...new Set([...toParams(search).keys()].filter(isOwnedKey))];
}

// ── Browser wrappers ─────────────────────────────────────────────────────

const hasWindow = () => typeof window !== 'undefined' && window.location && window.history;

export function readViewState() {
    return parseViewState(hasWindow() ? window.location.search : '');
}

// `replaceState`, never `m.route.set`: the router must not re-run its
// `onmatch` (which clears the chart registry and scrolls to the top) for a
// change that is not a navigation, and zoom steps must not fill the back
// button's history.
export function writeViewState(patch) {
    if (!hasWindow()) return;
    const cur = window.location.search;
    const next = applyViewState(cur, patch);
    if (next === cur) return;
    const url = `${window.location.pathname}${next}${window.location.hash}`;
    try {
        window.history.replaceState(window.history.state, '', url);
    } catch (_) {
        // A sandboxed document may refuse; the view is unaffected.
    }
}

export function clearViewState() {
    if (!hasWindow()) return;
    const anchors = {};
    for (const k of ownedKeysIn(window.location.search)) {
        if (k.startsWith(ANCHOR_PREFIX)) anchors[k.slice(ANCHOR_PREFIX.length)] = null;
    }
    writeViewState({ ...CLEAR_PATCH, anchors: { ...CLEAR_PATCH.anchors, ...anchors } });
}
