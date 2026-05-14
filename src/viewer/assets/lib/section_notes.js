// Returns null on empty items so callers can inline `&&`-style.
export function renderSectionNotes({ title, lead, items, formatItem }) {
    if (!Array.isArray(items) || items.length === 0) return null;
    return m('div.section-notes', [
        m('h3', title),
        lead && m('p', lead),
        m('ul', items.map(formatItem)),
    ]);
}
