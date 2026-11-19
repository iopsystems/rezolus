// The single-chart view's scope field: the uuids a chart's queries read,
// separated by commas; empty reads every recording.
import test from 'node:test';
import assert from 'node:assert/strict';
import { scopeText, parseScopeText } from '../src/viewer/assets/lib/features/scope_text.js';

test('a scope is shown as its uuids separated by commas', () => {
    assert.equal(scopeText(['u1', 'u2']), 'u1, u2');
    assert.equal(scopeText(null), '');
    assert.equal(scopeText(undefined), '');
});

test('the text reads back as the uuids it lists, in order and each once', () => {
    assert.deepEqual(parseScopeText(' u2 ,u1,, u2 '), ['u2', 'u1']);
    assert.deepEqual(parseScopeText(scopeText(['a', 'b'])), ['a', 'b']);
});

test('text listing no uuid is no scope', () => {
    assert.equal(parseScopeText(''), null);
    assert.equal(parseScopeText(' , '), null);
    assert.equal(parseScopeText(null), null);
});
