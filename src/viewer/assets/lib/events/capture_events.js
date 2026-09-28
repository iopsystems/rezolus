// Per-capture events and recording starts, for compare-mode alignment.
//
// The editable events list (events_store.js) is the BASELINE's, is
// persisted to localStorage, and a restored working set wins over the
// file's. Alignment needs something different: each capture's events as
// the file carries them, keyed by capture id, never overridden by a
// notebook. That is this store. It is filled by app.js whenever compare
// mode is (re)established, from every capture's `file_metadata` and
// `metadata` (`/api/v1/captures` names the ids; both backends serve the
// two endpoints per capture id), and cleared when compare mode ends.
//
// `resolveAnchor` turns a capture's anchor value into an absolute
// instant in seconds. An anchor is either a signed ms offset from the
// capture's recording start (the numeric form the notebook has always
// stored), or `{ kind }`: the first event of that kind in the capture's
// own events. A `{ kind }` with no such event falls back to the start
// and says so (`resolved: false`), which the compare badge shows.
//
// Pure: no DOM, no mithril, so it runs under node:test.

const eventsFromFileMetadata = (fileMetadata) => {
    const slot = fileMetadata?.events;
    if (Array.isArray(slot)) return slot.slice();
    if (slot && Array.isArray(slot.events)) return slot.events.slice();
    return [];
};
export { eventsFromFileMetadata };

export class CaptureContext {
    constructor() {
        this._byId = new Map();
    }

    /** Record a capture's events and recording start (seconds), from its metadata. */
    set(id, { events = [], startSec = null } = {}) {
        if (!id) return;
        this._byId.set(String(id), {
            events: Array.isArray(events) ? events.filter((e) => e && Number.isFinite(e.timestamp)) : [],
            startSec: Number.isFinite(startSec) ? startSec : null,
        });
    }

    get(id) {
        return this._byId.get(String(id));
    }

    ids() {
        return [...this._byId.keys()];
    }

    clear() {
        this._byId.clear();
    }

    /** The distinct non-empty event kinds one capture carries. */
    kindsFor(id) {
        const out = new Set();
        for (const e of this.get(id)?.events || []) {
            if (typeof e.kind === 'string' && e.kind.trim()) out.add(e.kind.trim());
        }
        return out;
    }

    /**
     * Event kinds present in EVERY listed capture, sorted. Only these can
     * align all captures; a kind missing from one capture leaves that
     * capture on its start, which the badge reports.
     */
    kindsSharedBy(ids) {
        const list = Array.isArray(ids) ? ids : [];
        if (list.length === 0) return [];
        let shared = null;
        for (const id of list) {
            const k = this.kindsFor(id);
            shared = shared === null ? k : new Set([...shared].filter((x) => k.has(x)));
        }
        return [...(shared || [])].sort();
    }

    /** Every kind any listed capture carries, sorted, with the ids missing it. */
    kindsAcross(ids) {
        const list = Array.isArray(ids) ? ids : [];
        const all = new Set();
        for (const id of list) for (const k of this.kindsFor(id)) all.add(k);
        return [...all].sort().map((kind) => ({
            kind,
            missing: list.filter((id) => !this.kindsFor(id).has(kind)),
        }));
    }
}

export const captureContext = new CaptureContext();

/** True for the `{ kind }` anchor form. */
export const isKindAnchor = (a) =>
    a != null && typeof a === 'object' && typeof a.kind === 'string' && a.kind.trim() !== '';

/**
 * Resolve a capture's anchor to an absolute instant.
 *
 * `base` is the capture's recording start from the context, else the
 * first fetched sample (`timeDataSec[0]`), else 0. A numeric anchor is
 * `base + ms/1000`. A `{ kind }` anchor is the first event of that kind
 * in the capture's events, or `base` with `resolved: false`.
 *
 * Returns `{ sec, resolved, reason? }`.
 */
export const resolveAnchor = (anchor, id, timeDataSec, ctx) => {
    const entry = ctx && typeof ctx.get === 'function' ? ctx.get(id) : undefined;
    const firstSample = Array.isArray(timeDataSec) && timeDataSec.length > 0 && Number.isFinite(timeDataSec[0])
        ? timeDataSec[0]
        : null;
    const base = entry?.startSec ?? firstSample ?? 0;

    if (isKindAnchor(anchor)) {
        const kind = anchor.kind.trim();
        const ev = (entry?.events || []).find((e) => e.kind === kind);
        if (ev) return { sec: ev.timestamp / 1e9, resolved: true };
        return { sec: base, resolved: false, reason: `no ${kind} event` };
    }
    const ms = Number(anchor);
    return { sec: base + (Number.isFinite(ms) ? ms : 0) / 1000, resolved: true };
};
