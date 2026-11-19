// The warning for a recording whose rows all sit at one timestamp (data.js).
import test from 'node:test';
import assert from 'node:assert/strict';
import { singleInstantNotice } from '../src/viewer/assets/lib/data.js';

// 2026-10-30T18:44:18Z, the one row of a recording that opened with every
// rate chart empty.
const AT = 1793385858.0019026;

test('a recording at one instant is named, with the instant', () => {
    const notice = singleInstantNotice({ status: 'success', data: { minTime: AT, maxTime: AT } });
    assert.match(notice, /^This recording holds a single sample, at 2026-10-30T18:44:18Z\./);
});

test('the label replaces the default subject', () => {
    const notice = singleInstantNotice({ minTime: AT, maxTime: AT }, 'The experiment recording');
    assert.match(notice, /^The experiment recording holds a single sample/);
});

test('snake_case extents are read too', () => {
    assert.ok(singleInstantNotice({ data: { min_time: AT, max_time: AT } }));
});

test('two samples, or no extent at all, is not warned about', () => {
    assert.equal(singleInstantNotice({ data: { minTime: AT, maxTime: AT + 1 } }), null);
    assert.equal(singleInstantNotice({ data: {} }), null);
    assert.equal(singleInstantNotice(null), null);
});

test('a recording with no time range, reported as 0/0, is not warned about', () => {
    assert.equal(singleInstantNotice({ data: { minTime: 0, maxTime: 0 } }), null);
});
