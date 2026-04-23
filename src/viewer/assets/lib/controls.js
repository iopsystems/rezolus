// ── Formatters ──────────────────────────────────────────────────────

const formatTime = (ms) => {
    const d = new Date(ms);
    const hh = String(d.getHours()).padStart(2, '0');
    const mm = String(d.getMinutes()).padStart(2, '0');
    const ss = String(d.getSeconds()).padStart(2, '0');
    return `${hh}:${mm}:${ss}`;
};

const formatDate = (ms) => {
    const d = new Date(ms);
    return d.toLocaleDateString([], { month: 'short', day: 'numeric' });
};

// Relative-time formatter for compare mode. `offsetMs` is the time since
// the baseline anchor (start of selection). Mirrors the axis-label style
// used elsewhere in compare mode: +XhYm, +XmYs, or +Xs.
const formatRelative = (offsetMs) => {
    const totalSec = Math.round(offsetMs / 1000);
    const sign = totalSec < 0 ? '-' : '+';
    const s = Math.abs(totalSec);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = s % 60;
    if (h > 0) return `${sign}${h}h${m}m`;
    if (m > 0) return `${sign}${m}m${sec}s`;
    return `${sign}${sec}s`;
};

/**
 * Parse a user-entered time string and return ms since epoch, or null on failure.
 * Accepts "HH:MM:SS" or "MMM DD HH:MM:SS" (e.g. "Mar 29 14:30:00").
 */
const parseTimeInput = (text, referenceMs) => {
    const ref = new Date(referenceMs);
    const timeOnly = text.match(/^(\d{1,2}):(\d{2}):(\d{2})$/);
    if (timeOnly) {
        const d = new Date(ref);
        d.setHours(parseInt(timeOnly[1], 10), parseInt(timeOnly[2], 10), parseInt(timeOnly[3], 10), 0);
        return d.getTime();
    }
    const full = text.match(/^(\w+)\s+(\d{1,2})\s+(\d{1,2}):(\d{2}):(\d{2})$/);
    if (full) {
        const months = ['jan','feb','mar','apr','may','jun','jul','aug','sep','oct','nov','dec'];
        const monthIdx = months.indexOf(full[1].toLowerCase());
        if (monthIdx === -1) return null;
        const d = new Date(ref);
        d.setMonth(monthIdx, parseInt(full[2], 10));
        d.setHours(parseInt(full[3], 10), parseInt(full[4], 10), parseInt(full[5], 10), 0);
        return d.getTime();
    }
    return null;
};

// Global time range bar — interactive minimap for zoom selection.
// Displays full experiment duration with a draggable selection window.
const TimeRangeBar = {
    oninit(vnode) {
        const zoom = vnode.attrs.chartsState?.globalZoom;
        vnode.state.barStart = zoom ? zoom.start : 0;
        vnode.state.barEnd = zoom ? zoom.end : 100;
        vnode.state.editing = null; // 'start' | 'end' | null
        vnode.state.editValue = '';
    },

    oncreate(vnode) {
        vnode.state.dragging = null; // 'left' | 'right' | 'move' | 'create'
        vnode.state.dragStartX = 0;
        vnode.state.dragStartLeft = 0;
        vnode.state.dragStartRight = 0;

        const chartsState = vnode.attrs.chartsState;
        const bar = vnode.dom;
        const track = bar.querySelector('.time-track');
        const getPercent = (clientX) => {
            const rect = track.getBoundingClientRect();
            return Math.max(0, Math.min(100, ((clientX - rect.left) / rect.width) * 100));
        };

        const getBarZoom = () => ({ start: vnode.state.barStart, end: vnode.state.barEnd });

        vnode.state.applyZoom = (start, end) => {
            // Enforce minimum 0.5% range
            if (end - start < 0.5) return;
            vnode.state.barStart = start;
            vnode.state.barEnd = end;
            // The single writer. setZoom writes zoomLevel/zoomSource/
            // globalZoom atomically and notifies every chart's zoom
            // subscriber; no need for a local forEach dispatch here.
            chartsState.setZoom({ start, end }, { source: 'global' });
            m.redraw();
        };

        const applyZoom = vnode.state.applyZoom;

        const onMouseDown = (e) => {
            if (e.button !== 0) return;
            if (!track.contains(e.target)) return;
            e.preventDefault();
            const pct = getPercent(e.clientX);
            const zoom = getBarZoom();
            const handleWidth = 3; // percent tolerance for handle hit

            if (Math.abs(pct - zoom.start) < handleWidth) {
                vnode.state.dragging = 'left';
            } else if (Math.abs(pct - zoom.end) < handleWidth) {
                vnode.state.dragging = 'right';
            } else if (pct > zoom.start && pct < zoom.end) {
                vnode.state.dragging = 'move';
                vnode.state.dragStartX = pct;
                vnode.state.dragStartLeft = zoom.start;
                vnode.state.dragStartRight = zoom.end;
            } else {
                // Click outside selection — start creating a new one
                vnode.state.dragging = 'create';
                vnode.state.dragStartX = pct;
                applyZoom(pct, pct + 0.5);
            }

            document.addEventListener('mousemove', onMouseMove);
            document.addEventListener('mouseup', onMouseUp);
        };

        const onMouseMove = (e) => {
            const pct = getPercent(e.clientX);
            const zoom = getBarZoom();

            if (vnode.state.dragging === 'left') {
                applyZoom(Math.min(pct, zoom.end - 0.5), zoom.end);
            } else if (vnode.state.dragging === 'right') {
                applyZoom(zoom.start, Math.max(pct, zoom.start + 0.5));
            } else if (vnode.state.dragging === 'move') {
                const delta = pct - vnode.state.dragStartX;
                let newStart = vnode.state.dragStartLeft + delta;
                let newEnd = vnode.state.dragStartRight + delta;
                const range = newEnd - newStart;
                if (newStart < 0) { newStart = 0; newEnd = range; }
                if (newEnd > 100) { newEnd = 100; newStart = 100 - range; }
                applyZoom(newStart, newEnd);
            } else if (vnode.state.dragging === 'create') {
                const anchor = vnode.state.dragStartX;
                applyZoom(Math.min(anchor, pct), Math.max(anchor, pct));
            }
        };

        const onMouseUp = () => {
            vnode.state.dragging = null;
            document.removeEventListener('mousemove', onMouseMove);
            document.removeEventListener('mouseup', onMouseUp);
        };

        bar.addEventListener('mousedown', onMouseDown);
        vnode.state.cleanup = () => {
            bar.removeEventListener('mousedown', onMouseDown);
            document.removeEventListener('mousemove', onMouseMove);
            document.removeEventListener('mouseup', onMouseUp);
        };
    },

    onremove(vnode) {
        if (vnode.state.cleanup) vnode.state.cleanup();
    },

    view(vnode) {
        const chartsState = vnode.attrs.chartsState;

        // Sync bar position from globalZoom only (not from local chart zoom,
        // which can carry undefined/NaN start/end when zooming by raw value).
        const zoom = chartsState?.globalZoom;
        if (zoom && (zoom.start !== vnode.state.barStart || zoom.end !== vnode.state.barEnd)) {
            vnode.state.barStart = zoom.start;
            vnode.state.barEnd = zoom.end;
        }

        const start = vnode.state.barStart;
        const end = vnode.state.barEnd;
        const startTime = vnode.attrs.start_time;
        const endTime = vnode.attrs.end_time;

        const totalDuration = endTime - startTime;
        if (!totalDuration || !isFinite(totalDuration)) return null;
        const selectedStartMs = startTime + (start / 100) * totalDuration;
        const selectedEndMs = startTime + (end / 100) * totalDuration;

        // Show date labels only when the selected start and end fall on different days
        const startDate = new Date(selectedStartMs);
        const endDate = new Date(selectedEndMs);
        const showDates = startDate.toDateString() !== endDate.toDateString();

        const commitEdit = (which) => {
            const text = vnode.state.editValue.trim();
            const refMs = which === 'start' ? selectedStartMs : selectedEndMs;
            const parsed = parseTimeInput(text, refMs);
            vnode.state.editing = null;
            if (parsed === null) return; // invalid input, discard

            // Clamp to recording bounds
            const clampedMs = Math.max(startTime, Math.min(endTime, parsed));
            const pct = ((clampedMs - startTime) / totalDuration) * 100;

            if (which === 'start') {
                vnode.state.applyZoom(Math.min(pct, end - 0.5), end);
            } else {
                vnode.state.applyZoom(start, Math.max(pct, start + 0.5));
            }
        };

        const startEditing = (which, currentMs) => {
            vnode.state.editing = which;
            vnode.state.editValue = showDates
                ? `${formatDate(currentMs)} ${formatTime(currentMs)}`
                : formatTime(currentMs);
        };

        const editInput = (which) => m('input.time-label-input', {
            value: vnode.state.editValue,
            oninput: (e) => { vnode.state.editValue = e.target.value; },
            onkeydown: (e) => {
                if (e.key === 'Enter') commitEdit(which);
                if (e.key === 'Escape') { vnode.state.editing = null; }
            },
            onblur: () => commitEdit(which),
            oncreate: (v) => { v.dom.focus(); v.dom.select(); },
        });

        const timeLabel = (which, ms) => {
            if (vnode.state.editing === which) {
                return editInput(which);
            }
            // In compare mode, render labels as relative offsets from the
            // recording's start; suppress the date prefix entirely.
            if (vnode.attrs.compareMode) {
                const baselineLabel = vnode.attrs.baselineAlias || 'baseline';
                return m('span.time-label', {
                    title: `Relative to ${baselineLabel} start`,
                }, formatRelative(ms - startTime));
            }
            return m('span.time-label', {
                ondblclick: () => startEditing(which, ms),
                title: 'Double-click to edit',
            }, [
                showDates && m('span.time-label-date', formatDate(ms)),
                formatTime(ms),
            ]);
        };

        const hidden = vnode.attrs.hidden;
        const hasLocalZoom = !hidden && chartsState?.zoomSource === 'local';

        return m('div.time-range-bar', {
            style: { visibility: hidden ? 'hidden' : '' },
        }, [
            timeLabel('start', selectedStartMs),
            m('div.time-track', [
                // Dimmed regions outside selection
                start > 0 && m('div.time-dim', { style: { left: '0%', width: `${start}%` } }),
                end < 100 && m('div.time-dim', { style: { left: `${end}%`, width: `${100 - end}%` } }),
                // Selection window
                m('div.time-selection', {
                    style: { left: `${start}%`, width: `${end - start}%` },
                }, [
                    m('div.time-handle.time-handle-left', [
                        m('span.time-handle-arrow.time-handle-arrow-left', '←'),
                        m('span.time-handle-arrow.time-handle-arrow-right', '→'),
                    ]),
                    m('div.time-handle.time-handle-right', [
                        m('span.time-handle-arrow.time-handle-arrow-left', '←'),
                        m('span.time-handle-arrow.time-handle-arrow-right', '→'),
                    ]),
                ]),
            ]),
            timeLabel('end', selectedEndMs),
            // "Match Selection" — appears when charts have a local zoom that differs from
            // the global time bar. Clicking snaps the global range to the chart zoom.
            hasLocalZoom && m('button.time-match-btn', {
                onclick: (e) => {
                    e.stopPropagation();
                    const localZoom = chartsState.zoomLevel;
                    let s = localZoom?.start;
                    let e2 = localZoom?.end;
                    // Local zoom via scroll gives raw ms values, not percentages
                    if ((s === undefined || isNaN(s)) && localZoom?.startValue !== undefined) {
                        const total = endTime - startTime;
                        s = Math.max(0, Math.min(100, ((localZoom.startValue - startTime) / total) * 100));
                        e2 = Math.max(0, Math.min(100, ((localZoom.endValue - startTime) / total) * 100));
                    }
                    if (s !== undefined && !isNaN(s)) {
                        vnode.state.applyZoom(s, e2);
                    }
                },
                title: 'Snap global time range to current chart zoom',
            }, 'Match Selection'),
            m('button.time-reset-btn', {
                onclick: (e) => {
                    e.stopPropagation();
                    vnode.state.barStart = 0;
                    vnode.state.barEnd = 100;
                    chartsState.resetAll();
                    m.redraw();
                },
                title: 'Reset to full time range',
                style: { visibility: (!hidden && (start > 0.1 || end < 99.9)) ? 'visible' : 'hidden' },
            }, 'Reset'),
        ]);
    },
};

// Granularity (step) selector — lets users override the auto-calculated query step.
const GRANULARITY_OPTIONS = [
    { value: '', label: 'Auto' },
    { value: '1', label: '1s' },
    { value: '5', label: '5s' },
    { value: '15', label: '15s' },
    { value: '60', label: '1m' },
];

const GranularitySelector = {
    view(vnode) {
        const value = vnode.attrs.value;
        const onChange = vnode.attrs.onChange;
        const hidden = vnode.attrs.hidden;

        return m('div.granularity-selector', {
            style: { visibility: hidden ? 'hidden' : '' },
        }, [
            m('label.granularity-label', 'Step'),
            m('select.granularity-select', {
                value: value == null ? '' : String(value),
                onchange: (e) => {
                    const val = e.target.value === '' ? null : parseInt(e.target.value, 10);
                    onChange(val);
                },
            }, GRANULARITY_OPTIONS.map(opt =>
                m('option', { value: opt.value }, opt.label),
            )),
        ]);
    },
};

export { TimeRangeBar, GranularitySelector };
