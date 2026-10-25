import { test } from 'node:test';
import assert from 'node:assert/strict';

// #1523: selected cgroup names reach the engine inside a PromQL string.
// promql-parser unquotes it (so a bare backslash escape is consumed) and
// compiles it as a regex; metriken-storage then compares each `|`-separated
// alternative with the name literally.

globalThis.document = globalThis.document || { documentElement: { getAttribute: () => null } };
globalThis.getComputedStyle = globalThis.getComputedStyle || (() => ({ getPropertyValue: () => '' }));
const { cgroupPattern, cgroupUnselectableReason } = await import('../src/viewer/assets/lib/features/cgroup_selector.js');

test('a plain name is used as it is', () => {
    assert.equal(cgroupPattern(['/system.slice/ssh.service']), '/system.slice/ssh.service');
    assert.equal(cgroupPattern(['/a', '/b']), '(/a|/b)');
    assert.equal(cgroupPattern([]), null);
});

test("a systemd \\x2d escape keeps its backslash through the PromQL string", () => {
    // The name contains a literal backslash: /system.slice/systemd-cryptsetup@luks\x2dabc.service
    const name = '/system.slice/systemd-cryptsetup@luks\\x2dabc.service';
    assert.equal(cgroupPattern([name]), '/system.slice/systemd-cryptsetup@luks\\\\x2dabc.service');
    assert.equal(cgroupUnselectableReason(name), null);
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

test('names the engine cannot match are left out and say why', () => {
    // Each of these is outside the allow-list. The engine cannot match most
    // ("|", lookarounds, \\0, \\c, unbalanced brackets, the "\\." that
    // match_pattern rewrites); /a{b and /a"b it could, and are excluded on
    // purpose.
    for (const name of ['/a|b', '/a[b', '/a(b', '*x', '/a"b', '/a(?=x)', '/a\\0', '/a\\cJ', '/a\\.b', '/a{b', '']) {
        assert.match(cgroupUnselectableReason(name), /^Cannot be selected/, name);
    }
    assert.equal(cgroupPattern(['/ok', '/a|b', '/a[b']), '/ok');
    assert.equal(cgroupPattern(['/a|b']), null);
});
