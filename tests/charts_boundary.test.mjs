import test from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join, normalize, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

// charts/ is copied into other projects (iopsystems/systemslab vendors it with
// svelte-ui/scripts/sync-rezolus-charts.mjs), so a chart module may refer only
// to files inside charts/, and may not use the viewer's Mithril global. What a
// chart needs from its host page comes through charts/host.js instead.

const CHARTS = join(dirname(fileURLToPath(import.meta.url)), '../src/viewer/assets/lib/charts');

// Every quoted or backtick string that looks like a module path: it starts
// with ./, ../ or / and ends in .js, .mjs, .css or .json, optionally followed
// by a query or fragment. That covers static imports and re-exports, import()
// with any quote, new URL(..., import.meta.url), workers and importScripts,
// whatever comments sit between the tokens. A path written in a comment is
// checked too; reword the comment if it trips this. Paths built by
// concatenation and other file types are not checked.
const PATH_LITERAL = /(['"`])((?:\.\.?\/|\/)[^'"`\s?#]*\.(?:m?js|css|json))(?:[?#][^'"`\s]*)?\1/g;

function jsFiles(dir) {
    return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
        e.isDirectory() ? jsFiles(join(dir, e.name))
            : e.name.endsWith('.js') && e.name !== 'echarts.min.js' ? [join(dir, e.name)] : []);
}

function escapingPaths(root) {
    const found = [];
    for (const file of jsFiles(root)) {
        const src = readFileSync(file, 'utf8');
        for (const [, , spec] of src.matchAll(PATH_LITERAL)) {
            const escapes = spec.startsWith('/')
                || relative(root, normalize(join(dirname(file), spec))).startsWith('..');
            if (escapes) found.push(`${relative(root, file)} -> ${spec}`);
        }
    }
    return found;
}

test('chart modules refer only to files inside charts/', () => {
    assert.deepEqual(escapingPaths(CHARTS), [],
        'add what the chart needs to charts/host.js instead of importing it from the viewer');
});

// Mithril's global `m`: a call, a Mithril member read from it (m.redraw,
// m?.route), `} = m` on one line, `typeof m`, or the global reached through
// window, self or globalThis. Local variables named `m` (event_markers.js,
// util/labels.js and util/family_math.js have them) are never called and
// never have a Mithril member name read from them, so none of these match.
const MITHRIL_USE = new RegExp([
    String.raw`(?:^|[^\w$.])m\s*\(`,
    String.raw`(?:^|[^\w$.])m\??\.(?:redraw|route|mount|render|request|trust|fragment|censor|buildPathname|parseQueryString|buildQueryString|parsePathname|jsonp|withAttr|prop|stream)\b`,
    String.raw`(?:^|[^\w$.])m\?\.`,
    String.raw`\}\s*=\s*m\b(?!\w)`,
    String.raw`\btypeof\s+m\b(?![\w$])`,
    String.raw`\b(?:window|self|globalThis)\.m\b(?![\w$])`,
].join('|'));

test('chart modules do not use the Mithril global', () => {
    const uses = [];
    for (const file of jsFiles(CHARTS)) {
        readFileSync(file, 'utf8').split('\n').forEach((line, i) => {
            if (MITHRIL_USE.test(line)) uses.push(`${relative(CHARTS, file)}:${i + 1}: ${line.trim()}`);
        });
    }
    assert.deepEqual(uses, [], 'take what the chart needs from charts/host.js instead');
});
