// A small CommonMark-subset renderer -> safe HTML, for the pack description
// preview. The launcher renders description_md with a real CommonMark library
// and explicitly ignores raw HTML; this covers the common subset (headings,
// emphasis, code, lists, quotes, links, images, rules) and never emits raw
// user HTML, so the preview cannot inject markup. Honest limits: it is a subset,
// not a full parser -- the operator's authored copy is simple marketing text.

function escapeHtml(s: string): string {
  return s.replace(
    /[&<>"']/g,
    (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c] ?? c,
  );
}

// Only allow benign URL shapes; anything else (javascript:, data:, etc.) -> '#'.
export function safeUrl(url: string): string {
  const u = url.trim();
  if (/^(https?:\/\/|mailto:|#|\/|\.{1,2}\/)/i.test(u)) return u;
  if (/^[\w.-]+(\/|$)/.test(u)) return u; // bare relative path
  return '#';
}

// The placeholder that stands in for an already-rendered span while the prose
// around it is escaped. A NUL cannot be typed, cannot survive a JSON round trip
// as anything else, and is stripped from the input below -- so unlike the
// printable `@@MD0@@` this replaced, no authored text can forge one. Writing
// that sequence used to make the restore pass substitute somebody else's link
// or image in its place, duplicating a span the author never wrote.
const MARK = '\u0000';

function renderInline(text: string): string {
  const stash: string[] = [];
  const keep = (html: string): string => {
    stash.push(html);
    return `${MARK}${stash.length - 1}${MARK}`;
  };

  let s = text.replaceAll(MARK, '');
  // Protect literal spans before escaping the surrounding prose.
  s = s.replace(/`([^`]+)`/g, (_m, code: string) => keep(`<code>${escapeHtml(code)}</code>`));
  s = s.replace(/!\[([^\]]*)\]\(([^)\s]+)\)/g, (_m, alt: string, url: string) =>
    keep(`<img alt="${escapeHtml(alt)}" src="${escapeHtml(safeUrl(url))}" />`),
  );
  s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_m, label: string, url: string) =>
    keep(
      `<a href="${escapeHtml(safeUrl(url))}" target="_blank" rel="noopener noreferrer">${escapeHtml(label)}</a>`,
    ),
  );

  s = escapeHtml(s);
  s = s
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/__([^_]+)__/g, '<strong>$1</strong>');
  s = s
    .replace(/\*([^*]+)\*/g, '<em>$1</em>')
    .replace(/(^|[^\w])_([^_]+)_(?=[^\w]|$)/g, '$1<em>$2</em>');

  return s.replace(/\u0000(\d+)\u0000/g, (_m, i: string) => stash[Number(i)] ?? '');
}

function isBlockStart(line: string): boolean {
  return (
    /^(#{1,6}\s|```|\s*>|\s*[-*+]\s|\s*\d+[.)]\s)/.test(line) ||
    /^\s*([-*_])(\s*\1){2,}\s*$/.test(line)
  );
}

/** Render a CommonMark subset to sanitised HTML. */
export function renderMarkdown(md: string): string {
  const lines = md.replace(/\r\n?/g, '\n').split('\n');
  const out: string[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    if (/^```/.test(line)) {
      const body: string[] = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) {
        body.push(lines[i]);
        i++;
      }
      i++; // closing fence
      out.push(`<pre><code>${escapeHtml(body.join('\n'))}</code></pre>`);
      continue;
    }

    const heading = line.match(/^(#{1,6})\s+(.*)$/);
    if (heading) {
      const level = heading[1].length;
      out.push(`<h${level}>${renderInline(heading[2].trim())}</h${level}>`);
      i++;
      continue;
    }

    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) {
      out.push('<hr />');
      i++;
      continue;
    }

    if (/^\s*>/.test(line)) {
      const body: string[] = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) {
        body.push(lines[i].replace(/^\s*>\s?/, ''));
        i++;
      }
      out.push(`<blockquote>${renderMarkdown(body.join('\n'))}</blockquote>`);
      continue;
    }

    if (/^\s*[-*+]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*[-*+]\s+/, ''));
        i++;
      }
      out.push(`<ul>${items.map((it) => `<li>${renderInline(it)}</li>`).join('')}</ul>`);
      continue;
    }

    if (/^\s*\d+[.)]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*\d+[.)]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*\d+[.)]\s+/, ''));
        i++;
      }
      out.push(`<ol>${items.map((it) => `<li>${renderInline(it)}</li>`).join('')}</ol>`);
      continue;
    }

    if (/^\s*$/.test(line)) {
      i++;
      continue;
    }

    const para: string[] = [];
    while (i < lines.length && !/^\s*$/.test(lines[i]) && !isBlockStart(lines[i])) {
      para.push(lines[i]);
      i++;
    }
    out.push(`<p>${renderInline(para.join(' '))}</p>`);
  }

  return out.join('\n');
}
