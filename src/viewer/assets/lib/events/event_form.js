// One-shot popover for adding an event from a chart-tooltip context.
// `openEventForm(opts)` mounts a mithril component to a fresh body div,
// anchors it to `opts.anchorEl.getBoundingClientRect()`, pre-fills
// fields from `opts.prefill`, and calls `opts.onSubmit(event)` with
// the constructed Event payload (matching `dashboard::events::Event`'s
// JSON shape).
//
// Dismissal: ESC, outside click, or Cancel. Submission: required
// description; chart_id is set when "Show only on this chart" stays
// checked (defaulted ON per spec).

const formatNsAsRfc3339 = (ns) => {
    if (!Number.isFinite(ns)) return '';
    return new Date(Math.round(ns / 1_000_000)).toISOString();
};

const parseRfc3339AsNs = (str) => {
    const ms = Date.parse(str);
    if (!Number.isFinite(ms)) return null;
    return ms * 1_000_000;
};

export function openEventForm({ anchorEl, prefill, onSubmit }) {
    if (!anchorEl) return;
    document.querySelectorAll('.event-form-overlay').forEach((n) => n.remove());

    const overlay = document.createElement('div');
    overlay.className = 'event-form-overlay';
    document.body.appendChild(overlay);

    const rect = anchorEl.getBoundingClientRect();
    const POPOVER_W = 320;
    const POPOVER_H_EST = 280;
    let top = rect.bottom + 8;
    if (top + POPOVER_H_EST > window.innerHeight) top = Math.max(8, rect.top - POPOVER_H_EST - 8);
    let left = rect.left;
    if (left + POPOVER_W > window.innerWidth) left = window.innerWidth - POPOVER_W - 8;
    if (left < 8) left = 8;

    let timestampStr = formatNsAsRfc3339(prefill.timestamp_ns);
    let description = '';
    let kind = '';
    let source = prefill.source || '';
    let node = prefill.node || '';
    let instance = prefill.instance || '';
    let onlyThisChart = true;
    let formError = '';

    const close = () => {
        document.removeEventListener('keydown', onKey, true);
        document.removeEventListener('mousedown', onClickOutside, true);
        m.mount(overlay, null);
        overlay.remove();
    };

    const onKey = (e) => {
        if (e.key === 'Escape') {
            e.preventDefault();
            close();
        }
    };
    const onClickOutside = (e) => {
        if (!overlay.contains(e.target)) close();
    };
    document.addEventListener('keydown', onKey, true);
    document.addEventListener('mousedown', onClickOutside, true);

    const submit = () => {
        if (!description.trim()) {
            formError = 'Description is required';
            m.redraw();
            return;
        }
        const ts = parseRfc3339AsNs(timestampStr);
        if (ts == null) {
            formError = 'Timestamp is not a valid RFC3339 / ISO-8601 string';
            m.redraw();
            return;
        }
        const event = {
            timestamp: ts,
            description: description.trim(),
        };
        if (kind.trim()) event.kind = kind.trim();
        if (source.trim()) event.source = source.trim();
        if (node.trim()) event.node = node.trim();
        if (instance.trim()) event.instance = instance.trim();
        if (onlyThisChart && prefill.chart_id) event.chart_id = prefill.chart_id;
        onSubmit(event);
        close();
    };

    const Form = {
        // POPOVER_H_EST is a guess; once mounted, measure the real
        // height and reseat the top edge so the Add button never falls
        // off the bottom of the viewport.
        oncreate: (vnode) => {
            const h = vnode.dom.getBoundingClientRect().height;
            const maxTop = window.innerHeight - h - 8;
            if (top > maxTop) {
                top = Math.max(8, maxTop);
                vnode.dom.style.top = top + 'px';
            }
        },
        view: () => m('div.event-form', {
            style: `position: fixed; top: ${top}px; left: ${left}px; width: ${POPOVER_W}px; z-index: 10000;`,
        }, [
            m('div.event-form-row', [
                m('label', 'Timestamp'),
                m('input', {
                    type: 'text',
                    value: timestampStr,
                    oninput: (e) => { timestampStr = e.target.value; formError = ''; },
                }),
            ]),
            m('div.event-form-row', [
                m('label', 'Description'),
                m('input', {
                    type: 'text',
                    value: description,
                    oninput: (e) => { description = e.target.value; formError = ''; },
                    autofocus: true,
                }),
            ]),
            formError ? m('div.event-form-error', formError) : null,
            m('div.event-form-row', [
                m('label', 'Kind'),
                m('input', {
                    type: 'text',
                    value: kind,
                    placeholder: 'e.g. restart, deploy, incident',
                    oninput: (e) => { kind = e.target.value; },
                }),
            ]),
            m('div.event-form-row', [
                m('label', 'Source'),
                m('input', { type: 'text', value: source, oninput: (e) => { source = e.target.value; } }),
            ]),
            m('div.event-form-row', [
                m('label', 'Node'),
                m('input', { type: 'text', value: node, oninput: (e) => { node = e.target.value; } }),
            ]),
            m('div.event-form-row', [
                m('label', 'Instance'),
                m('input', { type: 'text', value: instance, oninput: (e) => { instance = e.target.value; } }),
            ]),
            prefill.chart_id ? m('div.event-form-checkbox', [
                m('label', [
                    m('input', {
                        type: 'checkbox',
                        checked: onlyThisChart,
                        onchange: (e) => { onlyThisChart = e.target.checked; },
                    }),
                    ' Show only on this chart',
                ]),
            ]) : null,
            m('div.event-form-actions', [
                m('button', { onclick: close }, 'Cancel'),
                m('button.primary', { onclick: submit }, 'Add'),
            ]),
        ]),
    };
    m.mount(overlay, Form);
}

// Read-only "more information" popover for an event — the viewing
// counterpart of openEventForm. Shows every populated field; offers a
// Delete action (with inline confirm) only when `onDelete` is passed
// (Notebook). Anchored above the clicked bubble, flips below on clip.
export function openEventInfo({ anchorPoint, event, onDelete }) {
    if (!anchorPoint) return;
    document.querySelectorAll('.event-form-overlay, .event-info-overlay').forEach((n) => n.remove());

    const overlay = document.createElement('div');
    overlay.className = 'event-info-overlay';
    document.body.appendChild(overlay);

    let confirmingDelete = false;

    const close = () => {
        document.removeEventListener('keydown', onKey, true);
        document.removeEventListener('mousedown', onClickOutside, true);
        m.mount(overlay, null);
        overlay.remove();
    };

    const onKey = (e) => {
        if (e.key === 'Escape') {
            e.preventDefault();
            close();
        }
    };
    const onClickOutside = (e) => {
        if (!overlay.contains(e.target)) close();
    };
    document.addEventListener('keydown', onKey, true);
    document.addEventListener('mousedown', onClickOutside, true);

    // [label, value] pairs; blank values are dropped so the popover only
    // shows what the event actually carries.
    const rows = [
        ['Timestamp', formatNsAsRfc3339(event.timestamp)],
        ['Description', event.description],
        ['Kind', event.kind],
        ['Source', event.source],
        ['Node', event.node],
        ['Instance', event.instance],
    ].filter(([, v]) => v != null && String(v).trim() !== '');

    const POPOVER_W = 300;

    const Info = {
        oncreate: (vnode) => {
            const r = vnode.dom.getBoundingClientRect();
            let left = anchorPoint.x - r.width / 2;
            if (left < 8) left = 8;
            if (left + r.width > window.innerWidth) left = window.innerWidth - r.width - 8;
            let top = anchorPoint.y - r.height - 6;
            if (top < 8) top = anchorPoint.y + 10;
            vnode.dom.style.left = left + 'px';
            vnode.dom.style.top = top + 'px';
        },
        view: () => m('div.event-info', {
            style: `position: fixed; left: -9999px; top: -9999px; width: ${POPOVER_W}px; z-index: 10000;`,
        }, [
            ...rows.map(([label, value]) => m('div.event-info-row', [
                m('div.event-info-label', label),
                m('div.event-info-value', String(value)),
            ])),
            onDelete ? m('div.event-form-actions', [
                confirmingDelete
                    ? m('span.event-info-confirm', 'Delete this event?')
                    : null,
                m('button', { onclick: close }, 'Close'),
                confirmingDelete
                    ? m('button.danger', {
                        onclick: () => { onDelete(); close(); },
                    }, 'Confirm delete')
                    : m('button.danger', {
                        onclick: () => { confirmingDelete = true; m.redraw(); },
                    }, 'Delete'),
            ]) : m('div.event-form-actions', [
                m('button', { onclick: close }, 'Close'),
            ]),
        ]),
    };
    m.mount(overlay, Info);
}
