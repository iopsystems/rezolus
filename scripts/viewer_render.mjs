#!/usr/bin/env node
//
// Render a running viewer in a real browser and report what it actually drew.
//
// `tests/` asserts on payload shape: `viewer_smoke.sh` checks API envelopes, and
// the `.test.mjs` files exercise pure frontend logic with no DOM. Neither can
// see a chip strip that wrapped out of the navbar, a label the theme rendered at
// the disabled contrast tier, or a mithril keyed-fragment warning. This does.
//
// It lives in `scripts/` rather than `tests/` on purpose: CI runs
// `node --test tests/*.mjs`, and that glob takes every `.mjs` in the directory,
// not just `*.test.mjs` — a tool parked there is executed as a test and fails
// the suite.
//
// It is deliberately NOT wired into CI: it needs a browser on the machine and a
// puppeteer-core install that this repo does not carry (see the `viewer-render`
// skill for why). Run it by hand when a change moves pixels.
//
//   node scripts/viewer_render.mjs --url http://127.0.0.1:4299 \
//        --selector '.compare-badge' --out /tmp/badge.png
//
// Output is a JSON block on stdout plus two PNGs: the matched element, and the
// top band of the page so the element is seen in the layout it shares.
//
// Requires PUPPETEER_CORE to point at a puppeteer-core install, or one to be
// resolvable from the current directory. See `--help`.

import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import path from 'node:path';

const USAGE = `
Usage: node scripts/viewer_render.mjs --url <url> [options]

  --url <url>          Viewer to load (required), e.g. http://127.0.0.1:4299
  --selector <css>     Element to inspect and screenshot (default: body)
  --children <css>     Descendants to enumerate (default: direct children).
                       Use it to reach the node that actually carries the text
                       or title, e.g. '.compare-badge-label'.
  --out <path>         PNG path; a second "<name>-context.png" is also written
  --width <px>         Viewport width (default 1440). Vary it to find wrap points.
  --height <px>        Viewport height (default 900)
  --theme <name>       Force 'light' or 'dark' via the data-theme attribute
  --wait <css>         Extra selector to await before measuring
  --timeout <ms>       Navigation/selector timeout (default 30000)

Requires puppeteer-core and a Chrome/Chromium binary. Neither is a repo
dependency — this repo carries no JS toolchain on purpose. Install into a
scratch directory and point at it:

  mkdir -p /tmp/viewer-render && cd /tmp/viewer-render && npm i puppeteer-core
  PUPPETEER_CORE=/tmp/viewer-render/node_modules/puppeteer-core \\
  CHROME_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \\
  node scripts/viewer_render.mjs --url http://127.0.0.1:4299 --selector .compare-badge

Two things to get right when starting the viewer you point this at:

  REZOLUS_NO_OPEN=1   or every run pops a real browser tab on the desktop:
                      "rezolus view" opens one a second after binding.
  --features developer-mode
                      or the binary serves the frontend assets embedded at
                      COMPILE time and you screenshot a stale UI.

  REZOLUS_NO_OPEN=1 target/debug/rezolus view --listen 127.0.0.1:4299 out.rez &
`;

const args = process.argv.slice(2);
if (args.includes('--help') || args.includes('-h')) {
    console.log(USAGE);
    process.exit(0);
}

const arg = (name, fallback = undefined) => {
    const i = args.indexOf(`--${name}`);
    return i >= 0 && args[i + 1] ? args[i + 1] : fallback;
};

const url = arg('url');
if (!url) {
    console.error(USAGE);
    console.error('error: --url is required');
    process.exit(2);
}

const selector = arg('selector', 'body');
const childSelector = arg('children');
const out = arg('out');
const width = Number(arg('width', 1440));
const height = Number(arg('height', 900));
const theme = arg('theme');
const waitFor = arg('wait');
const timeout = Number(arg('timeout', 30000));

// ── Locate puppeteer-core ────────────────────────────────────────────────────
// Not a repo dependency, so resolve it from the environment first and fall back
// to whatever the working directory can see.
const resolvePuppeteer = () => {
    const explicit = process.env.PUPPETEER_CORE;
    if (explicit) {
        const pkg = path.join(explicit, 'package.json');
        try {
            const require = createRequire(pathToFileURL(pkg));
            const main = require(pkg).main || 'lib/puppeteer/puppeteer-core.js';
            return pathToFileURL(path.join(explicit, main)).href;
        } catch {
            return pathToFileURL(path.join(explicit, 'lib/puppeteer/puppeteer-core.js')).href;
        }
    }
    return 'puppeteer-core';
};

let puppeteer;
try {
    puppeteer = (await import(resolvePuppeteer())).default;
} catch (e) {
    console.error(USAGE);
    console.error(`error: could not load puppeteer-core (${e.message})`);
    process.exit(2);
}

// ── Locate a browser ─────────────────────────────────────────────────────────
// puppeteer-core ships no browser by design; use one already on the machine.
const CHROME_CANDIDATES = [
    process.env.CHROME_PATH,
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
].filter(Boolean);

const { existsSync } = await import('node:fs');
const executablePath = CHROME_CANDIDATES.find((p) => existsSync(p));
if (!executablePath) {
    console.error(`error: no Chrome/Chromium found. Set CHROME_PATH.\ntried:\n  ${CHROME_CANDIDATES.join('\n  ')}`);
    process.exit(2);
}

// ── Render ───────────────────────────────────────────────────────────────────
const browser = await puppeteer.launch({
    executablePath,
    headless: true,
    args: ['--no-sandbox', '--disable-gpu', '--hide-scrollbars'],
});

let exitCode = 0;
try {
    const page = await browser.newPage();
    // deviceScaleFactor 2 so text in the PNG is legible when a human reads it.
    await page.setViewport({ width, height, deviceScaleFactor: 2 });

    // Console noise and failed requests are findings, not decoration: a mithril
    // keyed-fragment violation or a 404 on a newly added module shows up here
    // and nowhere else.
    const consoleProblems = [];
    const failedRequests = [];
    page.on('console', (m) => {
        if (m.type() === 'error' || m.type() === 'warning') {
            consoleProblems.push(`${m.type()}: ${m.text()}`);
        }
    });
    page.on('pageerror', (e) => consoleProblems.push(`pageerror: ${e.message}`));
    page.on('response', (r) => {
        if (r.status() >= 400) failedRequests.push(`${r.status()} ${r.url()}`);
    });

    await page.goto(url, { waitUntil: 'networkidle2', timeout });
    if (waitFor) await page.waitForSelector(waitFor, { timeout });
    await page.waitForSelector(selector, { timeout });

    if (theme) {
        await page.evaluate((t) => document.documentElement.setAttribute('data-theme', t), theme);
        await new Promise((r) => setTimeout(r, 300));
    }

    const facts = await page.evaluate((sel, childSel) => {
        const el = document.querySelector(sel);
        const box = (n) => {
            const r = n.getBoundingClientRect();
            return { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) };
        };
        const cs = getComputedStyle(el);
        // Direct children by default, but the node carrying the text and the
        // title is often a grandchild (a label inside a chip) — and that is
        // exactly the node whose truncation and tooltip matter.
        const kids = childSel ? [...el.querySelectorAll(childSel)] : [...el.children];
        return {
            selector: sel,
            tag: el.tagName.toLowerCase(),
            classes: el.className,
            title: el.getAttribute('title'),
            box: box(el),
            style: {
                display: cs.display,
                flexWrap: cs.flexWrap,
                color: cs.color,
                background: cs.backgroundColor,
                fontSize: cs.fontSize,
            },
            // Distinct child row tops mean the content wrapped onto more than
            // one line — the thing a single screenshot at one width hides.
            rowTops: [...new Set(kids.map((k) => Math.round(k.getBoundingClientRect().top)))],
            childCount: kids.length,
            children: kids.slice(0, 24).map((k) => ({
                tag: k.tagName.toLowerCase(),
                classes: k.className,
                text: (k.textContent || '').trim().slice(0, 40),
                title: k.getAttribute('title'),
                color: getComputedStyle(k).color,
                box: box(k),
                // scrollWidth past the painted width means an ellipsis is
                // hiding text; if nothing carries a title, it is unrecoverable.
                truncated: k.scrollWidth > Math.ceil(k.getBoundingClientRect().width) + 1,
            })),
        };
    }, selector, childSelector);

    facts.viewport = { width, height };
    facts.theme = theme || (await page.evaluate(() => document.documentElement.getAttribute('data-theme') || 'dark'));
    facts.wrapped = facts.rowTops.length > 1;
    facts.consoleProblems = consoleProblems;
    facts.failedRequests = failedRequests;

    console.log(JSON.stringify(facts, null, 2));

    if (out) {
        const el = await page.$(selector);
        await el.screenshot({ path: out });
        // The band of page above and around the element, for layout context.
        const contextHeight = Math.max(80, facts.box.y + facts.box.h + 30);
        await page.screenshot({
            path: out.replace(/(\.png)?$/, '-context.png'),
            clip: { x: 0, y: 0, width, height: Math.min(contextHeight, height) },
        });
        console.error(`wrote ${out} and ${out.replace(/(\.png)?$/, '-context.png')}`);
    }
} catch (e) {
    console.error(`error: ${e.message}`);
    exitCode = 1;
} finally {
    await browser.close();
}

process.exit(exitCode);
