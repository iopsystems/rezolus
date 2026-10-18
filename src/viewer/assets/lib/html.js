// Escape text for interpolation into an HTML string (element content or a
// quoted attribute value). Recording-derived strings reach the viewer's
// innerHTML sinks and ECharts tooltip formatters, whose return value ECharts
// sets as innerHTML: plot titles and section names from section templates
// (which an agent can send in its stream handshake), KPI titles, series label
// values, capture labels, metric names. Any of them can carry markup, so
// every one is passed through this before it is concatenated into HTML.
// Text that the code itself formats as markup (formatDateTime's millisecond
// span, ECharts' `p.marker`) is not passed through it.
//
// null and undefined become the empty string; anything else is stringified.

const ESCAPES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

export function escapeHtml(value) {
    if (value == null) return '';
    return String(value).replace(/[&<>"']/g, (c) => ESCAPES[c]);
}
