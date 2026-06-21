import test from 'node:test';
import assert from 'node:assert/strict';
import { composeAbReportPrefix } from '../src/viewer/assets/lib/selection/ab_filename.js';

test('joins two simple aliases', () => {
    assert.equal(composeAbReportPrefix('before', 'after'), 'before_vs_after');
});

test('strips trailing .parquet from each side', () => {
    assert.equal(
        composeAbReportPrefix('cachecannon.parquet', 'AB_base_pin.parquet'),
        'cachecannon_vs_AB_base_pin',
    );
});

test('strips trailing .parquet.ab.tar', () => {
    assert.equal(
        composeAbReportPrefix('foo.parquet.ab.tar', 'bar.parquet.ab.tar'),
        'foo_vs_bar',
    );
});

test('strips trailing .ab.tar without .parquet middle', () => {
    assert.equal(composeAbReportPrefix('foo.ab.tar', 'bar.ab.tar'), 'foo_vs_bar');
});

test('strips directory components, keeps basename', () => {
    assert.equal(
        composeAbReportPrefix('/tmp/foo/baseline.parquet', 'C:\\runs\\exp.parquet'),
        'baseline_vs_exp',
    );
});

test('falls back to slot literals on empty / nullish input', () => {
    assert.equal(composeAbReportPrefix(null, undefined), 'baseline_vs_experiment');
    assert.equal(composeAbReportPrefix('', ''), 'baseline_vs_experiment');
    assert.equal(composeAbReportPrefix('  ', '\t'), 'baseline_vs_experiment');
});

test('replaces whitespace and unsafe chars with underscores', () => {
    assert.equal(
        composeAbReportPrefix('my run 01', 'exp v2'),
        'my_run_01_vs_exp_v2',
    );
});

test('collapses runs of underscores after sanitization', () => {
    assert.equal(composeAbReportPrefix('a  b', 'c__d'), 'a_b_vs_c_d');
});
