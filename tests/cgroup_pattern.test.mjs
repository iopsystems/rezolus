// Unit tests for the cgroup-selection SQL placeholder substitution.
// Pinpoints the /cgroups regression triage path: pre-fix, the deferred
// `__SELECTED_CGROUPS__` token was left in the SQL stream — DuckDB
// parser blew up, every aggregate / individual cgroup chart silently
// produced zero rows. With the substitution wired in, no-selection
// resolves to `('')` (matches main viewer's PromQL-side empty-pattern
// semantics) and explicit selection splices the IN-list verbatim.

import test from 'node:test';
import assert from 'node:assert/strict';
import { substituteCgroupPattern } from '../src/viewer/assets/lib/data.js';

test('passes through SQL that does not reference the placeholder', () => {
    const sql = 'SELECT 1 AS t, 2::DOUBLE AS v FROM _src';
    assert.equal(substituteCgroupPattern(sql, "('a')"), sql);
});

test('empty / unset pattern substitutes to the empty IN-list', () => {
    // Aggregate-side SQL uses `NOT IN (__SELECTED_CGROUPS__)` so
    // empty-selection ⇒ `NOT IN ('')` ⇒ include everything.
    const sql = `SELECT t FROM x WHERE name NOT IN __SELECTED_CGROUPS__`;
    const expected = `SELECT t FROM x WHERE name NOT IN ('')`;
    assert.equal(substituteCgroupPattern(sql, null), expected);
    assert.equal(substituteCgroupPattern(sql, undefined), expected);
    assert.equal(substituteCgroupPattern(sql, ''), expected);
});

test('explicit pattern splices the literal IN-list verbatim', () => {
    const sql = `SELECT t FROM x WHERE name IN __SELECTED_CGROUPS__`;
    assert.equal(
        substituteCgroupPattern(sql, "('foo','bar')"),
        `SELECT t FROM x WHERE name IN ('foo','bar')`,
    );
});

test('replaces every occurrence (aggregate + individual sometimes both fire)', () => {
    const sql = `... NOT IN __SELECTED_CGROUPS__ ... OR id IN __SELECTED_CGROUPS__`;
    assert.equal(
        substituteCgroupPattern(sql, "('a')"),
        `... NOT IN ('a') ... OR id IN ('a')`,
    );
});

test('handles empty / null sql defensively', () => {
    assert.equal(substituteCgroupPattern('', "('a')"), '');
    assert.equal(substituteCgroupPattern(null, "('a')"), null);
    assert.equal(substituteCgroupPattern(undefined, "('a')"), undefined);
});
