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

const RFC3339_FORMATTER = (nsTimestamp) => {
    if (!Number.isFinite(nsTimestamp)) return '';
    const ms = Math.round(nsTimestamp / 1_000_000);
    return new Date(ms).toISOString();
};

// Parse RFC3339 back to ns. Returns null on failure so the submit
// handler can show inline feedback rather than silently using the
// pre-filled value.
const parseRfc3339Ns = (str) => {
    const ms = Date.parse(str);
    if (!Number.isFinite(ms)) return null;
    return ms * 1_000_000;
};

export function openEventForm({ anchorEl, prefill, onSubmit }) {
    if (!anchorEl) return;
    // Tear down any existing form (single-instance — reopening from
    // another chart cancels the prior one).
    document.querySelectorAll('.event-form-overlay').forEach((n) => n.remove());

    const overlay = document.createElement('div');
    overlay.className = 'event-form-overlay';
    document.body.appendChild(overlay);

    const rect = anchorEl.getBoundingClientRect();
    // Anchor below the link; if it would clip the viewport bottom,
    // flip above. Width clamps to 320px or available space.
    const POPOVER_W = 320;
    const POPOVER_H_EST = 320;  // rough — recalculated below if needed
    let top = rect.bottom + 8;
    if (top + POPOVER_H_EST > window.innerHeight) top = Math.max(8, rect.top - POPOVER_H_EST - 8);
    let left = rect.left;
    if (left + POPOVER_W > window.innerWidth) left = window.innerWidth - POPOVER_W - 8;
    if (left < 8) left = 8;

    let timestampStr = RFC3339_FORMATTER(prefill.timestamp_ns);
    let description = '';
    let kind = '';
    let source = prefill.source || '';
    let node = prefill.node || '';
    let instance = prefill.instance || '';
    let onlyThisChart = true;
    let descError = '';

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
            descError = 'Description is required';
            m.redraw();
            return;
        }
        const ts = parseRfc3339Ns(timestampStr);
        if (ts == null) {
            descError = 'Timestamp is not a valid RFC3339 string';
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
        view: () => m('div.event-form', {
            style: `position: fixed; top: ${top}px; left: ${left}px; width: ${POPOVER_W}px; z-index: 10000;`,
        }, [
            m('div.event-form-row', [
                m('label', 'Timestamp'),
                m('input', {
                    type: 'text',
                    value: timestampStr,
                    oninput: (e) => { timestampStr = e.target.value; },
                }),
            ]),
            m('div.event-form-row', [
                m('label', 'Description'),
                m('input', {
                    type: 'text',
                    value: description,
                    oninput: (e) => { description = e.target.value; descError = ''; },
                    autofocus: true,
                }),
            ]),
            descError ? m('div.event-form-error', descError) : null,
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
