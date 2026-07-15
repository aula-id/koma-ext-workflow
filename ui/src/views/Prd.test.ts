import { describe, it, expect } from 'vitest';
import { escapeHtml, renderMarkdownSafe } from './Prd';

describe('escapeHtml', () => {
  it('escapes the five HTML-significant characters', () => {
    expect(escapeHtml(`<script>&"'</script>`)).toBe(
      '&lt;script&gt;&amp;&quot;&#39;&lt;/script&gt;',
    );
  });
});

describe('renderMarkdownSafe', () => {
  it('never emits a live <script> tag from raw HTML in the source', () => {
    const out = renderMarkdownSafe('<script>alert(1)</script>\n\nSafe **bold** text.');
    expect(out).not.toContain('<script>');
    expect(out).toContain('&lt;script&gt;alert(1)&lt;/script&gt;');
    expect(out).toContain('<strong>bold</strong>');
  });

  it('blocks javascript: and data: link schemes, keeps http(s)/mailto', () => {
    const out = renderMarkdownSafe(
      '[bad](javascript:alert(1)) [bad2](data:text/html,evil) [ok](https://example.com) [mail](mailto:a@b.com)',
    );
    expect(out).not.toContain('javascript:');
    expect(out).not.toContain('data:text/html');
    expect(out).toContain('href="https://example.com"');
    expect(out).toContain('href="mailto:a@b.com"');
  });

  it('neutralizes an attribute-breakout attempt inside a link URL', () => {
    const out = renderMarkdownSafe('[x](https://e.com/"onmouseover="alert(1))');
    expect(out).not.toMatch(/onmouseover="alert/);
  });

  it('renders headers, lists, and fenced code blocks', () => {
    const out = renderMarkdownSafe('# Title\n\n- one\n- two\n\n```\nraw <b> code\n```');
    expect(out).toContain('<h1>Title</h1>');
    expect(out).toContain('<li>one</li>');
    expect(out).toContain('<li>two</li>');
    expect(out).toContain('<pre><code>raw &lt;b&gt; code</code></pre>');
  });

  it('handles an empty PRD without throwing', () => {
    expect(renderMarkdownSafe('')).toBe('');
  });
});

describe('renderMarkdownSafe: tables, rules, ordered lists', () => {
  it('renders a GFM table with escaped inline content', () => {
    const md = '| Layer | Choice |\n|---|---|\n| UI | Tailwind **v4** |\n| DB | <script>x</script> |';
    const html = renderMarkdownSafe(md);
    expect(html).toContain('<table><thead><tr><th>Layer</th><th>Choice</th></tr></thead>');
    expect(html).toContain('<td>UI</td>');
    expect(html).toContain('<strong>v4</strong>');
    expect(html).toContain('&lt;script&gt;');
    expect(html).not.toContain('<script>');
  });

  it('pads short rows to the header width instead of collapsing columns', () => {
    const html = renderMarkdownSafe('| A | B |\n|---|---|\n| only |');
    expect(html).toContain('<td>only</td><td></td>');
  });

  it('renders --- as a horizontal rule, not a paragraph', () => {
    const html = renderMarkdownSafe('above\n\n---\n\nbelow');
    expect(html).toContain('<hr />');
    expect(html).not.toContain('<p>---</p>');
  });

  it('renders ordered lists and keeps ul/ol separate', () => {
    const html = renderMarkdownSafe('1. first\n2. second\n\n- bullet');
    expect(html).toContain('<ol>');
    expect(html).toContain('<li>first</li>');
    expect(html).toContain('<ul>');
  });

  it('a lone pipe line without a separator row stays a paragraph', () => {
    const html = renderMarkdownSafe('a | b | c');
    expect(html).toContain('<p>a | b | c</p>');
    expect(html).not.toContain('<table>');
  });
});
