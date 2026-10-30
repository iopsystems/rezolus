import test from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join, normalize, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

// charts/ is copied into other projects (iopsystems/systemslab vendors it with
// svelte-ui/scripts/sync-rezolus-charts.mjs), so a chart module may refer only
// to files inside charts/. What a chart needs from its host page comes through
// charts/host.js instead.
//
// chart.js and compare.js still import from the viewer. ALLOWED lists exactly
// those imports, and a listed import that is no longer there fails the test.

const CHARTS = join(dirname(fileURLToPath(import.meta.url)), '../src/viewer/assets/lib/charts');

const ALLOWED = new Set([
    'chart.js -> ../ui/theme.js',
    'chart.js -> ../events/events_store.js',
    'chart.js -> ../events/event_form.js',
    'compare.js -> ../data.js',
    'compare.js -> ../events/capture_events.js',
]);

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
    const unexpected = escapingPaths(CHARTS).filter((e) => !ALLOWED.has(e));
    assert.deepEqual(unexpected, [],
        'add what the chart needs to charts/host.js instead of importing it from the viewer');
});

test('every allowed escaping import still exists', () => {
    const found = new Set(escapingPaths(CHARTS));
    const stale = [...ALLOWED].filter((e) => !found.has(e));
    assert.deepEqual(stale, [], 'remove these from ALLOWED');
});
