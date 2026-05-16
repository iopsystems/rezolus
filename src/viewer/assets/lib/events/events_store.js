// In-memory store of one-off event annotations attached to the loaded
// recording. Seeded from `fileMetadata.events` on load; appended to via
// the chart-tooltip "Add Event" form; persisted to the parquet footer
// when Save as Report runs (via selection.js::buildPayload).
//
// Pure module — no DOM, no mithril import — so it stays testable under
// node:test the same way compare_math.js and selection_migration.js do.
export class EventsStore {
    constructor() {
        this._events = [];
        this._subs = new Set();
    }

    seedFromMetadata(fileMetadata) {
        const slot = fileMetadata?.events;
        let arr = [];
        if (Array.isArray(slot)) {
            arr = slot;
        } else if (slot && Array.isArray(slot.events)) {
            // Actual parquet wire shape: {"events":[...]} wrapper object
            arr = slot.events;
        }
        this._events = arr.slice();
        this._notify();
    }

    add(event) {
        this._events.push(event);
        this._notify();
    }

    // Remove the first event matching timestamp + description. Match by
    // value (not reference) so callers don't have to round-trip the
    // exact object — ECharts' internal data clones, in particular, lose
    // identity by the time we get a click event back.
    remove({ timestamp, description }) {
        const idx = this._events.findIndex(
            (e) => e.timestamp === timestamp && (e.description || '') === (description || ''),
        );
        if (idx === -1) return false;
        this._events.splice(idx, 1);
        this._notify();
        return true;
    }

    all() {
        return this._events.slice();
    }

    // Replace the whole set (used by the localStorage restore path so
    // an unsaved working set survives a page refresh).
    replaceAll(events) {
        this._events = Array.isArray(events) ? events.slice() : [];
        this._notify();
    }

    clear() {
        this._events = [];
        this._notify();
    }

    subscribe(fn) {
        this._subs.add(fn);
        return () => { this._subs.delete(fn); };
    }

    // Per-chart visibility filter. `chart` carries a `chartId` and a
    // `scope: { source?, node?, instance? }`. Each event participates
    // when (a) its chart_id either matches or is absent and (b) every
    // populated scope field on the event matches the chart's scope.
    // Event fields left blank match anything for that field.
    filterForChart({ chartId, scope } = {}) {
        const s = scope || {};
        return this._events.filter((e) => {
            if (e.chart_id && e.chart_id !== chartId) return false;
            if (e.source && s.source && e.source !== s.source) return false;
            if (e.node && s.node && e.node !== s.node) return false;
            if (e.instance && s.instance && e.instance !== s.instance) return false;
            return true;
        });
    }

    _notify() {
        for (const fn of this._subs) fn();
    }
}

// Singleton instance used by the running viewer. Tests construct their
// own EventsStore directly to avoid cross-test bleed.
export const eventsStore = new EventsStore();
