import test from 'node:test';
import assert from 'node:assert/strict';
import { buildDefaultQuery } from '../src/viewer/assets/lib/charts/metric_types.js';

test('counter → rate over default window', () => {
  assert.match(buildDefaultQuery({ name: 'http_requests_total', metric_type: 'counter' }),
    /^rate\(http_requests_total\[\d+[smh]\]\)$/);
});
test('gauge → raw', () => {
  assert.equal(buildDefaultQuery({ name: 'queue_depth', metric_type: 'gauge' }), 'queue_depth');
});
test('histogram → percentiles', () => {
  assert.match(buildDefaultQuery({ name: 'req_latency', metric_type: 'histogram' }),
    /^histogram_quantiles\(\[.*\], req_latency\)$/);
});
