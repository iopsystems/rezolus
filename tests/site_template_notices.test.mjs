// The static site's adapter (site/viewer/lib/viewer_api.js) logs the template
// notices `init_templates` returns. A capture's templates are initialised more
// than once in a session (attach, compare, re-attach), so each distinct notice
// must reach the console once, not once per call.
import test from 'node:test';
import assert from 'node:assert/strict';
import { ViewerApi } from '../site/viewer/lib/viewer_api.js';

test('each distinct template notice is logged once', () => {
    const notices = {
        baseline: '["source a: does not parse"]',
        experiment: '["source b: does not parse","source a: does not parse"]',
    };
    ViewerApi.setRegistry({
        has: () => true,
        init_templates: (capture) => notices[capture],
    });
    const warned = [];
    const original = console.warn;
    console.warn = (msg) => warned.push(msg);
    try {
        ViewerApi.initTemplates('[]', 'baseline');
        ViewerApi.initTemplates('[]', 'experiment');
        ViewerApi.initTemplates('[]', 'baseline');
    } finally {
        console.warn = original;
        ViewerApi.setRegistry(null);
    }
    assert.deepEqual(warned, ['source a: does not parse', 'source b: does not parse']);
});
