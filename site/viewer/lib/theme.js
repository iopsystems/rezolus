/**
 * Theme management — persists the user's light/dark preference in localStorage.
 *
 * The active theme is applied as a `data-theme` attribute on <html>.
 * CSS variables are overridden in style.css under `[data-theme="light"]`.
 *
 * `themeVersion` is incremented on each toggle so chart components can detect
 * a theme change in their `onupdate` lifecycle hook and re-render accordingly.
 */

const STORAGE_KEY = 'rezolus-theme';

export let themeVersion = 0;

function getPreferred() {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === 'light' || stored === 'dark') return stored;
    return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

function apply(theme) {
    document.documentElement.setAttribute('data-theme', theme);
    document.querySelector('meta[name="theme-color"]')?.setAttribute(
        'content',
        theme === 'light' ? '#f6f8fa' : '#0a0e14',
    );
}

export function initTheme() {
    apply(getPreferred());
}

export function toggleTheme() {
    const current = document.documentElement.getAttribute('data-theme') || 'dark';
    const next = current === 'dark' ? 'light' : 'dark';
    localStorage.setItem(STORAGE_KEY, next);
    apply(next);
    themeVersion++;
    m.redraw();
}

export function currentTheme() {
    return document.documentElement.getAttribute('data-theme') || 'dark';
}
