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
        vnode.state.barStart = 0;
        vnode.state.barEnd = 100;
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
            chartsState.zoomLevel = { start, end };
            chartsState.zoomSource = 'global';
            chartsState.charts.forEach(chart => {
                chart.dispatchAction({ type: 'dataZoom', start, end });
                chart._rescaleYAxis();
            });
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
        const start = vnode.state.barStart;
        const end = vnode.state.barEnd;
        const startTime = vnode.attrs.start_time;
        const endTime = vnode.attrs.end_time;

        const totalDuration = endTime - startTime;
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
            return m('span.time-label', {
                ondblclick: () => startEditing(which, ms),
                title: 'Double-click to edit',
            }, [
                showDates && m('span.time-label-date', formatDate(ms)),
                formatTime(ms),
            ]);
        };

        return m('div.time-range-bar', [
            timeLabel('start', selectedStartMs),
            m('div.time-track', [
                // Dimmed regions outside selection
                start > 0 && m('div.time-dim', { style: { left: '0%', width: `${start}%` } }),
                end < 100 && m('div.time-dim', { style: { left: `${end}%`, width: `${100 - end}%` } }),
                // Selection window
                m('div.time-selection', {
                    style: { left: `${start}%`, width: `${end - start}%` },
                }, [
                    m('div.time-handle.time-handle-left'),
                    m('div.time-handle.time-handle-right'),
                ]),
            ]),
            timeLabel('end', selectedEndMs),
            m('button.time-reset-btn', {
                onclick: (e) => {
                    e.stopPropagation();
                    vnode.state.barStart = 0;
                    vnode.state.barEnd = 100;
                    chartsState.resetAll();
                    m.redraw();
                },
                title: 'Reset to full time range',
                style: { visibility: (start > 0.1 || end < 99.9) ? 'visible' : 'hidden' },
            }, 'Reset'),
        ]);
    },
};

export { TimeRangeBar };
