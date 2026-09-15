// A small Markdown-subset renderer -> safe HTML, for the pack description
// preview. It covers the common subset (headings, emphasis, code, lists,
// quotes, links, images, rules, tables) plus one HTML construct, and escapes
// everything else, so the preview cannot inject markup.
//
// A description is written once and read in two places: here, and in the
// launcher, which parses it with a real library on the GFM flavour and renders
// the result into its own widget tree. That one can afford constructs this one
// cannot, because nothing it produces reaches a DOM. Whatever it draws and this
// does not, a reader sees as literal text on the site.
//
// That is why tables and `<details>` are here. Without tables the pipes read as
// prose and a table arrives as one run-on paragraph; without `<details>` the
// tags themselves print. Both were being read that way on published packs.
//
// `<details>` is the only markup let through, by name, and the reason it can be
// is that nothing of the author's is passed along with it: the tags are matched,
// dropped, and re-emitted by this file, and everything inside goes back through
// the same escaping as the rest. This is a whitelist of two tags, not a door for
// HTML. Nothing else gets one without the same treatment.

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

/**
 * Whether this line ends the paragraph above it, `next` being the line after.
 *
 * A table needs both lines to decide: a pipe alone is ordinary punctuation, and
 * only the rule underneath makes the line above a header. Answering on the first
 * line alone made every sentence containing a `|` a block nothing then consumed,
 * which is a parser that stops advancing rather than a paragraph that renders
 * oddly.
 */
function isBlockStart(line: string, next: string | undefined): boolean {
  return (
    /^(#{1,6}\s|```|\s*>|\s*[-*+]\s|\s*\d+[.)]\s)/.test(line) ||
    /^\s*([-*_])(\s*\1){2,}\s*$/.test(line) ||
    /^\s*<details>/.test(line) ||
    (isTableRow(line) && next !== undefined && isTableRule(next))
  );
}

/** A pipe-delimited row: at least one `|` with something either side of it. */
function isTableRow(line: string): boolean {
  return /^\s*\|?[^|\n]*\|/.test(line) && line.includes('|');
}

/** The `|---|:--:|` rule that makes the line above it a header row. */
function isTableRule(line: string): boolean {
  return /^\s*\|?\s*:?-{1,}:?\s*(\|\s*:?-{1,}:?\s*)*\|?\s*$/.test(line) && line.includes('-');
}

/**
 * The cells of one row, with the optional leading and trailing pipe dropped.
 *
 * An escaped `\|` is a literal pipe inside a cell rather than a boundary, so it
 * is protected before the split and restored after: a mod named `A|B` in a table
 * would otherwise silently become two columns.
 */
function tableCells(line: string): string[] {
  const kept = line.replace(/\\\|/g, '\u0001');
  return kept
    .trim()
    .replace(/^\|/, '')
    .replace(/\|$/, '')
    .split('|')
    .map((c) => c.replaceAll('\u0001', '|').trim());
}

/** `left` / `center` / `right` per column, from the rule row; `null` when unset. */
function tableAlign(rule: string): (string | null)[] {
  return tableCells(rule).map((c) => {
    const left = c.startsWith(':');
    const right = c.endsWith(':');
    if (left && right) return 'center';
    if (right) return 'right';
    if (left) return 'left';
    return null;
  });
}

/**
 * A GFM table, or `null` when what follows the first row is not a rule -- a
 * paragraph that merely contains a pipe is prose, and must stay prose.
 *
 * Ragged rows are kept rather than rejected: a row short of cells is padded and
 * a long one is cut to the header's width, so a typo costs a blank cell instead
 * of the whole block falling back to a run-on paragraph.
 */
function renderTable(lines: string[], start: number): { html: string; next: number } | null {
  if (!isTableRow(lines[start]) || start + 1 >= lines.length) return null;
  if (!isTableRule(lines[start + 1])) return null;

  const head = tableCells(lines[start]);
  const align = tableAlign(lines[start + 1]);
  let i = start + 2;
  const body: string[][] = [];
  while (i < lines.length && isTableRow(lines[i]) && !/^\s*$/.test(lines[i])) {
    body.push(tableCells(lines[i]));
    i++;
  }

  const cell = (tag: string, text: string, col: number): string => {
    const a = align[col] ? ` style="text-align:${align[col]}"` : '';
    return `<${tag}${a}>${renderInline(text)}</${tag}>`;
  };
  const headHtml = head.map((c, n) => cell('th', c, n)).join('');
  const bodyHtml = body
    .map((row) => {
      const cells = head.map((_, n) => cell('td', row[n] ?? '', n)).join('');
      return `<tr>${cells}</tr>`;
    })
    .join('');
  const html =
    `<table><thead><tr>${headHtml}</tr></thead>` +
    (bodyHtml ? `<tbody>${bodyHtml}</tbody>` : '') +
    `</table>`;
  return { html, next: i };
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

    // <details>: matched by name, re-emitted by us, contents re-parsed. The
    // author's own angle brackets never survive this -- see the file header.
    const open = line.match(/^\s*<details>\s*$/);
    const openWithSummary = line.match(/^\s*<details>\s*<summary>(.*?)<\/summary>\s*$/);
    if (open || openWithSummary) {
      let j = i + 1;
      let summary = openWithSummary ? openWithSummary[1] : '';
      if (!openWithSummary && j < lines.length) {
        const only = lines[j].match(/^\s*<summary>(.*?)<\/summary>\s*$/);
        if (only) {
          summary = only[1];
          j++;
        }
      }
      const body: string[] = [];
      let depth = 1;
      while (j < lines.length) {
        if (/^\s*<details>/.test(lines[j])) depth++;
        if (/^\s*<\/details>\s*$/.test(lines[j])) {
          depth--;
          if (depth === 0) break;
        }
        body.push(lines[j]);
        j++;
      }
      // An unclosed <details> is not a block: falling through leaves the tags
      // visible, which is what an author needs to see to fix it.
      if (depth === 0) {
        const head = summary.trim() ? `<summary>${renderInline(summary)}</summary>` : '';
        out.push(`<details>${head}${renderMarkdown(body.join('\n'))}</details>`);
        i = j + 1;
        continue;
      }
    }

    const table = renderTable(lines, i);
    if (table) {
      out.push(table.html);
      i = table.next;
      continue;
    }

    if (/^\s*$/.test(line)) {
      i++;
      continue;
    }

    // The first line is taken before the test, not after it. Every other branch
    // above consumes what it matched; this one is where anything unmatched ends
    // up, so a line it declined would leave the loop standing still.
    const para: string[] = [lines[i]];
    i++;
    while (i < lines.length && !/^\s*$/.test(lines[i]) && !isBlockStart(lines[i], lines[i + 1])) {
      para.push(lines[i]);
      i++;
    }
    out.push(`<p>${renderInline(para.join(' '))}</p>`);
  }

  return out.join('\n');
}
