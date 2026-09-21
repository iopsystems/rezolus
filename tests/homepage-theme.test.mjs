import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const html = readFileSync(new URL('../site/index.html', import.meta.url), 'utf8');

function page({ dark = false, saved = null, blocked = false } = {}) {
    const script = html.match(/<script id="homepage-theme">([\s\S]*?)<\/script>/)?.[1];
    assert.ok(script, 'homepage initializes its theme before rendering');
    const classes = new Set();
    const attrs = {};
    const listeners = {};
    const icon = { textContent: '' };
    const button = {
        setAttribute: (key, value) => { attrs[key] = value; },
        querySelector: () => icon,
        addEventListener: (event, fn) => { listeners[event] = fn; },
    };
    const root = {
        classList: { toggle: (name, on) => on ? classes.add(name) : classes.delete(name) },
        style: {},
    };
    let ready;
    let changed;
    let stored = saved;
    runInNewContext(script, {
        document: {
            documentElement: root,
            getElementById: () => button,
            addEventListener: (event, fn) => { ready = fn; },
        },
        window: {
            matchMedia: () => ({ matches: dark, addEventListener: (event, fn) => { changed = fn; } }),
        },
        localStorage: {
            getItem: () => { if (blocked) throw new Error('blocked'); return stored; },
            setItem: (key, value) => { if (blocked) throw new Error('blocked'); stored = value; },
        },
    });
    return {
        root, attrs, classes,
        ready: () => ready(),
        click: () => listeners.click(),
        system: (matches) => changed({ matches }),
        stored: () => stored,
    };
}

test('system preference is applied before rendering and follows changes', () => {
    const p = page({ dark: true });
    assert.equal(p.root.style.colorScheme, 'dark');
    assert.ok(p.classes.has('dark'));
    p.ready();
    assert.equal(p.attrs['aria-pressed'], 'true');
    p.system(false);
    assert.equal(p.root.style.colorScheme, 'light');
    assert.equal(p.attrs['aria-pressed'], 'false');
});

test('saved choice overrides system preference and clicks persist both modes', () => {
    const p = page({ dark: true, saved: 'light' });
    p.ready();
    assert.equal(p.root.style.colorScheme, 'light');
    p.system(true);
    assert.equal(p.root.style.colorScheme, 'light');
    p.click();
    assert.equal(p.stored(), 'dark');
    assert.equal(p.attrs['aria-pressed'], 'true');
    p.click();
    assert.equal(p.stored(), 'light');
    assert.equal(p.attrs['aria-pressed'], 'false');
    const reload = page({ saved: 'dark' });
    assert.equal(reload.root.style.colorScheme, 'dark');
});

test('toggle works without storage and system changes respect the manual choice', () => {
    const p = page({ blocked: true });
    p.ready();
    p.click();
    assert.equal(p.root.style.colorScheme, 'dark');
    p.system(false);
    assert.equal(p.root.style.colorScheme, 'dark');
});

test('invalid saved preference falls back to the system', () => {
    const p = page({ saved: 'invalid', dark: true });
    assert.equal(p.root.style.colorScheme, 'dark');
});
