import { test } from 'node:test';
import assert from 'node:assert/strict';
import { displayBudget } from '../src/viewer/assets/lib/data.js';

// Without a DOM the pixel budget is 500, so the bucket budget is 125.
const meta = { interval: 1 };

test('a long range gets one bucket per 4 px', () => {
    // 2277 samples: native / 5 = 456 buckets would be allowed, 125 fit the chart.
    assert.equal(displayBudget(meta, 0, 2277), 125);
});

test('a short range keeps at least five samples per bucket, floored at 48', () => {
    assert.equal(displayBudget(meta, 0, 400), 80);
    assert.equal(displayBudget(meta, 0, 100), 48);
});
