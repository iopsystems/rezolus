// A chart's scope as the single-chart view shows and edits it: the uuids of
// the sources its queries read, separated by commas. Empty text is no
// scope, which reads every recording.

// `scope` (an array of uuids, or null) as the text of the scope field.
export const scopeText = (scope) => (Array.isArray(scope) ? scope.join(', ') : '');

// The text of the scope field as a scope: the uuids it lists, in order and
// each once, or null when it lists none.
export const parseScopeText = (text) => {
    const uuids = [];
    for (const part of String(text ?? '').split(',')) {
        const uuid = part.trim();
        if (uuid && !uuids.includes(uuid)) uuids.push(uuid);
    }
    return uuids.length > 0 ? uuids : null;
};
