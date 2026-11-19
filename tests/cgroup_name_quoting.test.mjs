import { test } from 'node:test';
import assert from 'node:assert/strict';

// #1523: selected cgroup names reach the engine inside a PromQL string.
// promql-parser unquotes it (so a bare backslash escape is consumed), and the
// engine matches the result as a regular expression anchored at both ends.

globalThis.document = globalThis.document || { documentElement: { getAttribute: () => null } };
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { cgroupPattern, cgroupUnselectableReason } = await import('../src/viewer/assets/lib/features/cgroup_selector.js');

// The regex a pattern is once a PromQL string unquotes it, anchored as the
// engine anchors it.
const asEngineRegex = (pattern) => new RegExp('^(?:' + pattern.replace(/\\(.)/g, '$1') + ')$');

test('a name is escaped as a regex, then quoted', () => {
    assert.equal(cgroupPattern(['/system.slice/ssh.service']), '/system\\\\.slice/ssh\\\\.service');
    assert.equal(cgroupPattern(['/a', '/b']), '(/a|/b)');
    assert.equal(cgroupPattern([]), null);
});

test("a systemd \\x2d escape keeps its backslash through the PromQL string and the regex", () => {
    // The name contains a literal backslash: /system.slice/systemd-cryptsetup@luks\x2dabc.service
    const name = '/system.slice/systemd-cryptsetup@luks\\x2dabc.service';
    assert.equal(
        cgroupPattern([name]),
        '/system\\\\.slice/systemd-cryptsetup@luks\\\\\\\\x2dabc\\\\.service',
    );
    assert.equal(cgroupUnselectableReason(name), null);
});

test('a pattern matches its names and nothing that differs from them', () => {
    const names = [
        '/system.slice/ssh.service',
        '/system.slice/systemd-cryptsetup@luks\\x2dabc.service',
        '/a:b@c=d,e~f%g-h_i.j\\x2dk',
    ];
    const re = asEngineRegex(cgroupPattern(names));
    for (const name of names) assert.ok(re.test(name), name);
    for (const other of [
        '/systemXslice/ssh.service',
        '/system.slice/systemd-cryptsetup@luks-abc.service',
        '/system.slice/ssh.service/child',
        'x/system.slice/ssh.service',
    ]) {
        assert.ok(!re.test(other), other);
    }
});

test('every allowed character, and a mix of real names, is selectable', () => {
    for (const name of [
        '/system.slice/ssh.service',
        '.../machine-qemu-2-guest.scope/libvirt/vcpu3',
        '/system.slice/docker-0123abcdef.scope',
        '/kubepods.slice/kubepods-burstable.slice/pod1234_5678',
        '/a:b@c=d,e~f%g-h_i.j\\x2dk',
    ]) {
        assert.equal(cgroupUnselectableReason(name), null, name);
    }
});

test('names outside the allow-list are left out and say why', () => {
    for (const name of ['/a|b', '/a[b', '/a(b', '*x', '/a"b', '/a(?=x)', '/a\\0', '/a\\cJ', '/a\\.b', '/a{b', '']) {
        assert.match(cgroupUnselectableReason(name), /^Cannot be selected/, name);
    }
    assert.equal(cgroupPattern(['/ok', '/a|b', '/a[b']), '/ok');
    assert.equal(cgroupPattern(['/a|b']), null);
});
