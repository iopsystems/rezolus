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

// `range` ({ start, end } seconds) as percentages of the bar's span
// (`start_time`/`end_time`, milliseconds), or null without one.
const rangePercent = ({ range, start_time: t0, end_time: t1 }) => {
    const total = t1 - t0;
    if (!range || !(total > 0)) return null;
    const pct = (s) => Math.max(0, Math.min(100, ((s * 1000 - t0) / total) * 100));
    return { start: pct(range.start), end: pct(range.end) };
};

// Global time range bar — interactive minimap for zoom selection.
// Displays the whole recording (`start_time`..`end_time`, ms) with the window
// the charts are fetched for (`range`) selected. A drag sets a new window
// through `onRangeChange` on mouse release, a typed time on Enter, and Match
// Selection on click; Reset (`onRangeReset`) returns to the whole recording.
const TimeRangeBar = {
    oninit(vnode) {
        const zoom = rangePercent(vnode.attrs);
        vnode.state.barStart = zoom ? zoom.start : 0;
        vnode.state.barEnd = zoom ? zoom.end : 100;
        vnode.state.attrs = vnode.attrs;
        vnode.state.editing = null; // 'start' | 'end' | null
        vnode.state.editValue = '';
    },

    oncreate(vnode) {
        vnode.state.dragging = null; // 'left' | 'right' | 'move' | 'create'
        vnode.state.dragStartX = 0;
        vnode.state.dragStartLeft = 0;
        vnode.state.dragStartRight = 0;

        const bar = vnode.dom;
        const track = bar.querySelector('.time-track');
        const getPercent = (clientX) => {
            const rect = track.getBoundingClientRect();
            return Math.max(0, Math.min(100, ((clientX - rect.left) / rect.width) * 100));
        };

        const getBarZoom = () => ({ start: vnode.state.barStart, end: vnode.state.barEnd });

        vnode.state.applyZoom = (start, end) => {
            if (end - start < 0.5) return;
            vnode.state.barStart = start;
            vnode.state.barEnd = end;
            // Moves only the bar; commit() sets the charts' window.
            m.redraw();
        };

        // Set the charts' window to the selection, or back to the whole
        // recording when the selection covers it.
        vnode.state.commit = () => {
            const a = vnode.state.attrs;
            const { barStart: s, barEnd: e } = vnode.state;
            if (s <= 0.1 && e >= 99.9) {
                if (a.range) a.onRangeReset?.();
                return;
            }
            const total = a.end_time - a.start_time;
            a.onRangeChange({
                start: (a.start_time + (s / 100) * total) / 1000,
                end: (a.start_time + (e / 100) * total) / 1000,
            });
        };

        const applyZoom = vnode.state.applyZoom;

        const onMouseDown = (e) => {
            if (e.button !== 0) return;
            if (!track.contains(e.target)) return;
            e.preventDefault();
            const pct = getPercent(e.clientX);
            const zoom = getBarZoom();
            vnode.state.dragFrom = zoom;
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
            const from = vnode.state.dragging != null && vnode.state.dragFrom;
            vnode.state.dragging = null;
            document.removeEventListener('mousemove', onMouseMove);
            document.removeEventListener('mouseup', onMouseUp);
            // A click that moved nothing leaves the window as it is.
            if (from && (from.start !== vnode.state.barStart || from.end !== vnode.state.barEnd)) {
                vnode.state.commit();
            }
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

        vnode.state.attrs = vnode.attrs;
        // The selection is the charts' window, except mid-drag.
        if (!vnode.state.dragging) {
            const pct = rangePercent(vnode.attrs);
            vnode.state.barStart = pct ? pct.start : 0;
            vnode.state.barEnd = pct ? pct.end : 100;
        }

        const start = vnode.state.barStart;
        const end = vnode.state.barEnd;
        const startTime = vnode.attrs.start_time;
        const endTime = vnode.attrs.end_time;

        const totalDuration = endTime - startTime;
        if (!totalDuration || !isFinite(totalDuration)) return null;
        const selectedStartMs = startTime + (start / 100) * totalDuration;
        const selectedEndMs = startTime + (end / 100) * totalDuration;

        const startDate = new Date(selectedStartMs);
        const endDate = new Date(selectedEndMs);
        const showDates = startDate.toDateString() !== endDate.toDateString();

        const commitEdit = (which) => {
            // Enter commits and removes the input, whose blur would commit
            // again; Escape clears `editing` so its blur commits nothing.
            if (vnode.state.editing !== which) return;
            const text = vnode.state.editValue.trim();
            const refMs = which === 'start' ? selectedStartMs : selectedEndMs;
            const parsed = parseTimeInput(text, refMs);
            vnode.state.editing = null;
            if (parsed === null) return;

            const clampedMs = Math.max(startTime, Math.min(endTime, parsed));
            const pct = ((clampedMs - startTime) / totalDuration) * 100;

            if (which === 'start') {
                vnode.state.applyZoom(Math.min(pct, end - 0.5), end);
            } else {
                vnode.state.applyZoom(start, Math.max(pct, start + 0.5));
            }
            vnode.state.commit();
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
                start > 0 && m('div.time-dim', { style: { left: '0%', width: `${start}%` } }),
                end < 100 && m('div.time-dim', { style: { left: `${end}%`, width: `${100 - end}%` } }),
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
                    const toBar = (ms) => Math.max(0, Math.min(100, ((ms - startTime) / totalDuration) * 100));
                    let s;
                    let e2;
                    if (Number.isFinite(localZoom?.start)) {
                        // Percentages of the charts' x axis, which spans the
                        // fetched window (`range`), not the whole recording.
                        const range = vnode.attrs.range;
                        const w0 = range ? range.start * 1000 : startTime;
                        const w1 = range ? range.end * 1000 : endTime;
                        s = toBar(w0 + (localZoom.start / 100) * (w1 - w0));
                        e2 = toBar(w0 + (localZoom.end / 100) * (w1 - w0));
                    } else if (localZoom?.startValue !== undefined) {
                        // Local zoom via scroll gives raw ms values.
                        s = toBar(localZoom.startValue);
                        e2 = toBar(localZoom.endValue);
                    }
                    if (s !== undefined && !isNaN(s)) {
                        vnode.state.applyZoom(s, e2);
                        vnode.state.commit();
                    }
                },
                title: 'Snap global time range to current chart zoom',
            }, 'Match Selection'),
            m('button.time-reset-btn', {
                onclick: (e) => {
                    e.stopPropagation();
                    vnode.state.barStart = 0;
                    vnode.state.barEnd = 100;
                    vnode.attrs.onRangeReset?.();
                    m.redraw();
                },
                title: 'Reset to full time range',
                style: {
                    visibility: (!hidden && (vnode.attrs.range || start > 0.1 || end < 99.9))
                        ? 'visible' : 'hidden',
                },
            }, 'Reset'),
        ]);
    },
};

// Granularity (step) selector — lets users override the auto-calculated query
// step. Offered steps start at the recording's own sampling interval: a step
// finer than the cadence has nothing left to resolve, and a recording made at
// `--interval 100ms` has useful choices well below one second.
const GRANULARITY_LADDER = [0.01, 0.05, 0.1, 0.25, 0.5, 1, 5, 15, 60];

const formatStep = (secs) => {
    if (secs < 1) return `${Math.round(secs * 1000)}ms`;
    if (secs < 60) return `${Number(secs.toFixed(3))}s`;
    return `${Number((secs / 60).toFixed(3))}m`;
};

const granularityOptions = (interval) => {
    const native = (Number.isFinite(interval) && interval > 0) ? interval : 1;
    const steps = GRANULARITY_LADDER.filter((s) => s >= native);
    // A recording whose cadence isn't on the ladder (250ms, 2s) still gets its
    // own native step as the first choice.
    if (!steps.length || Math.abs(steps[0] - native) > 1e-9) steps.unshift(native);
    return [
        { value: '', label: 'Auto' },
        ...steps.map((s) => ({ value: String(s), label: formatStep(s) })),
    ];
};

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
                    // parseFloat, not parseInt: a sub-second step is a fraction
                    // of a second and parseInt would truncate 0.1 to 0.
                    const val = e.target.value === '' ? null : parseFloat(e.target.value);
                    onChange(val);
                },
            }, granularityOptions(vnode.attrs.interval).map(opt =>
                m('option', { value: opt.value }, opt.label),
            )),
        ]);
    },
};

// Time mode selector — how rate/irate points are placed in time.
//   Aligned (grid, default): grid-aligned per-step rate with uncertainty bands,
//                            comparable across recordings.
//   Raw:                     points at the real sample timestamps (un-aligned) —
//                            for jitter/cadence and single-recording analysis.
const TIME_MODE_OPTIONS = [
    { value: 'grid', label: 'Aligned' },
    { value: 'raw', label: 'Raw' },
];

const TimeModeSelector = {
    view(vnode) {
        const value = vnode.attrs.value || 'grid';
        const onChange = vnode.attrs.onChange;
        const hidden = vnode.attrs.hidden;
        // Compare mode forces 'grid' (Raw is un-alignable across recordings).
        const disabled = !!vnode.attrs.disabled;

        return m('div.granularity-selector', {
            style: { visibility: hidden ? 'hidden' : '' },
        }, [
            m('label.granularity-label', 'Time'),
            m('select.granularity-select', {
                value: disabled ? 'grid' : value,
                disabled,
                title: disabled
                    ? 'Compare uses aligned time (Raw is not comparable across recordings)'
                    : 'Aligned: grid-aligned per-step rate with bands. Raw: real sample timestamps.',
                onchange: (e) => onChange(e.target.value),
            }, TIME_MODE_OPTIONS.map(opt =>
                m('option', { value: opt.value }, opt.label),
            )),
        ]);
    },
};

export { TimeRangeBar, GranularitySelector, TimeModeSelector, granularityOptions };
