import test from 'node:test';
import assert from 'node:assert/strict';
import { renderMarkdown, renderMarkdownInline } from '../src/viewer/assets/lib/ui/markdown.js';

test('empty / nullish input yields empty string', () => {
    assert.equal(renderMarkdown(''), '');
    assert.equal(renderMarkdown(null), '');
    assert.equal(renderMarkdown(undefined), '');
});

test('plain text becomes a paragraph', () => {
    assert.equal(renderMarkdown('hello world'), '<p>hello world</p>');
});

test('escapes HTML before any markdown transform (XSS)', () => {
    const out = renderMarkdown('<img src=x onerror=alert(1)> & "q"');
    assert.ok(!out.includes('<img'));
    assert.ok(out.includes('&lt;img'));
    assert.ok(out.includes('&amp;'));
    assert.ok(out.includes('&quot;'));
});

test('bold and italic', () => {
    assert.equal(renderMarkdown('**bold**'), '<p><strong>bold</strong></p>');
    assert.equal(renderMarkdown('*it*'), '<p><em>it</em></p>');
    assert.equal(renderMarkdown('_it_'), '<p><em>it</em></p>');
});

test('inline code is escaped and not further parsed', () => {
    assert.equal(renderMarkdown('`**not bold**`'), '<p><code>**not bold**</code></p>');
});

test('headings level 1-6', () => {
    assert.equal(renderMarkdown('# H1'), '<h1>H1</h1>');
    assert.equal(renderMarkdown('###### H6'), '<h6>H6</h6>');
    // 7 hashes is not a heading
    assert.equal(renderMarkdown('####### x'), '<p>####### x</p>');
});

test('unordered list', () => {
    assert.equal(
        renderMarkdown('- a\n- b'),
        '<ul><li>a</li><li>b</li></ul>',
    );
});

test('ordered list', () => {
    assert.equal(
        renderMarkdown('1. a\n2. b'),
        '<ol><li>a</li><li>b</li></ol>',
    );
});

test('blockquote', () => {
    assert.equal(renderMarkdown('> quoted'), '<blockquote>quoted</blockquote>');
});

test('fenced code block keeps content verbatim + escaped, no inline parsing', () => {
    const out = renderMarkdown('```\n**x** <b>\n```');
    assert.equal(out, '<pre><code>**x** &lt;b&gt;\n</code></pre>');
});

test('safe link', () => {
    assert.equal(
        renderMarkdown('[rz](https://rezolus.com)'),
        '<p><a href="https://rezolus.com" target="_blank" rel="noopener noreferrer">rz</a></p>',
    );
});

test('relative + hash + mailto links allowed', () => {
    assert.ok(renderMarkdown('[a](/x)').includes('href="/x"'));
    assert.ok(renderMarkdown('[a](#sec)').includes('href="#sec"'));
    assert.ok(renderMarkdown('[a](mailto:x@y.z)').includes('href="mailto:x@y.z"'));
});

test('javascript: and data: link schemes are neutered to plain text', () => {
    const js = renderMarkdown('[x](javascript:alert(1))');
    assert.ok(!js.includes('href="javascript'));
    assert.ok(js.includes('x'));
    const data = renderMarkdown('[x](data:text/html,abc)');
    assert.ok(!data.includes('href="data:'));
});

test('paragraphs separated by blank lines', () => {
    assert.equal(renderMarkdown('a\n\nb'), '<p>a</p><p>b</p>');
});

test('single newline inside a paragraph becomes a line break', () => {
    assert.equal(renderMarkdown('a\nb'), '<p>a<br>b</p>');
});

test('mixed document: heading, list, paragraph', () => {
    const md = '# Title\n\n- one\n- two\n\nsome **text**';
    assert.equal(
        renderMarkdown(md),
        '<h1>Title</h1><ul><li>one</li><li>two</li></ul><p>some <strong>text</strong></p>',
    );
});

// ── inline mode (chart titles) ─────────────────────────────────────

test('inline: no block wrapper, just formatted text', () => {
    assert.equal(renderMarkdownInline('plain'), 'plain');
    assert.equal(renderMarkdownInline('**b** and *i*'), '<strong>b</strong> and <em>i</em>');
    assert.equal(renderMarkdownInline('`c`'), '<code>c</code>');
});

test('inline: escapes HTML (XSS)', () => {
    const out = renderMarkdownInline('<b>x</b> & "q"');
    assert.ok(!out.includes('<b>'));
    assert.ok(out.includes('&lt;b&gt;'));
    assert.ok(out.includes('&amp;'));
});

test('inline: newlines collapse to spaces, no block elements', () => {
    assert.equal(renderMarkdownInline('a\nb'), 'a b');
    // a leading "# " is NOT a heading in inline mode
    assert.equal(renderMarkdownInline('# not a heading'), '# not a heading');
    // list markers are literal
    assert.equal(renderMarkdownInline('- not a list'), '- not a list');
});

test('inline: safe link, unsafe scheme neutered', () => {
    assert.equal(
        renderMarkdownInline('[r](https://rezolus.com)'),
        '<a href="https://rezolus.com" target="_blank" rel="noopener noreferrer">r</a>',
    );
    assert.ok(!renderMarkdownInline('[x](javascript:alert(1))').includes('href="javascript'));
});

test('inline: empty/nullish yields empty string', () => {
    assert.equal(renderMarkdownInline(''), '');
    assert.equal(renderMarkdownInline(null), '');
    assert.equal(renderMarkdownInline(undefined), '');
});
