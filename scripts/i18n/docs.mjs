// Docs translations: which pages are missing or out of date, and whether a translated
// page keeps the English page's structure (code, components, links and anchors).
//
// English pages live in docs/src/content/docs/; a translation of `setup/install.mdx`
// lives at `<locale>/setup/install.mdx`. Its frontmatter `sourceHash` records which
// version of the English page it was translated from; when the English page changes,
// the translation is stale. Stale pages are reported, never failed.
import { createHash } from 'node:crypto';

export const docs = {
  content: 'docs/src/content/docs',
};

/** Short content hash used for `sourceHash`. */
export const hash = text => createHash('sha256').update(text).digest('hex').slice(0, 12);

/** Splits a page into its frontmatter fields (top-level `key: value` only) and body. */
export function frontmatter(text) {
  const match = text.match(/^---\n([\s\S]*?)\n---\n?/);
  if (!match) return { fields: {}, body: text };
  const fields = {};
  for (const line of match[1].split('\n')) {
    const field = line.match(/^([A-Za-z]\w*):\s*(.*)$/);
    if (field) fields[field[1]] = field[2].replace(/^(['"])(.*)\1$/, '$2');
  }
  return { fields, body: text.slice(match[0].length) };
}

/** Sets `sourceHash` in a page's frontmatter. */
export function stamp(text, sourceHash) {
  const match = text.match(/^---\n([\s\S]*?)\n---\n?/);
  if (!match) return `---\nsourceHash: ${sourceHash}\n---\n${text}`;
  const lines = match[1].split('\n').filter(line => !line.startsWith('sourceHash:'));
  return `---\n${[...lines, `sourceHash: ${sourceHash}`].join('\n')}\n---\n${text.slice(match[0].length)}`;
}

const fence = /^(```+|~~~+)([^\n]*)\n([\s\S]*?)^\1[ \t]*$/gm;

/** Fenced code blocks as `{ info, code }`. */
export const codeBlocks = body => [...body.matchAll(fence)].map(match => ({ info: match[2].trim(), code: match[3] }));

/**
 * Code with the parts a translator may change set aside: comments (`# …`, `// …`
 * after whitespace or at line start) and the contents of quoted strings. Commands,
 * flags, keys and identifiers must still match English.
 */
export const codeSkeleton = code => code.split('\n').map(line => line
  .replace(/"(?:[^"\\]|\\.)*"/g, '""')
  .replace(/'(?:[^'\\]|\\.)*'/g, "''")
  .replace(/(^|\s)(#|\/\/).*$/, '')
  .trimEnd()).join('\n').trimEnd();

/** Mermaid diagrams and blocks with no language (or `text`) hold prose such as labelled trees and key tables. */
const proseBlock = info => ['', 'text', 'txt', 'mermaid'].includes(info.split(/\s/)[0]);

/** The body with code blocks and inline code removed, so prose checks ignore code. */
const prose = body => body.replace(fence, '').replace(/`[^`\n]*`/g, '``');

/** `import` lines, with relative paths reduced to their target so `../` depth does not matter. */
export const imports = body => prose(body).split('\n').filter(line => /^import\s/.test(line))
  .map(line => line.replace(/(['"])(?:\.\.\/)+/, '$1…/')).sort();

/** MDX components used, by name. */
export const components = body => [...new Set([...prose(body).matchAll(/<([A-Z][A-Za-z0-9]*)[\s/>]/g)].map(match => match[1]))].sort();

/** Markdown links as `{ text, url }`, outside code blocks. */
export const links = body => [...prose(body).matchAll(/\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g)].map(match => match[1]);

/**
 * Heading slugs as Astro makes them (github-slugger): lower case, punctuation removed,
 * spaces to hyphens, and `-1`, `-2` … appended to repeats.
 */
function slugger() {
  const seen = new Map();
  return {
    slug(text) {
      const base = text.toLowerCase().trim().replace(/[^\p{L}\p{M}\p{N}\p{Pc} -]/gu, '').replace(/ /g, '-');
      let slug = base;
      for (let n = seen.get(base) ?? 0; seen.has(slug); n++) slug = `${base}-${n + 1}`, seen.set(base, n + 1);
      seen.set(slug, 0);
      return slug;
    },
  };
}

/** Link targets on a page: heading IDs (an explicit `\{#id}` or a slug of the text) and `id="…"` attributes. */
export function headingIds(body) {
  const slugs = slugger();
  const ids = [...prose(body).matchAll(/\sid=["']([^"']+)["']/g)].map(match => match[1]);
  for (const match of prose(body).matchAll(/^(#{2,6})[ \t]+(.*?)[ \t]*$/gm)) {
    const explicit = match[2].match(/\\?\{#([A-Za-z0-9_-]+)\}$/);
    if (explicit) {
      ids.push(explicit[1]);
      slugs.slug(explicit[1]);
    } else {
      ids.push(slugs.slug(match[2].replace(/`/g, '').replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')));
    }
  }
  return ids;
}

/** Resolves a link from `page` (a path like `setup/install.mdx`) to `{ page, anchor }`, or null if external. */
export function resolveLink(url, page) {
  if (/^[a-z]+:/i.test(url) || url.startsWith('//')) return null;
  const [path, anchor = ''] = url.split('#');
  if (!path) return { page, anchor };
  if (/\.(png|svg|jpe?g|gif|webp|mp4|json|txt)$/i.test(path)) return null;
  let target;
  if (path.startsWith('/')) target = path.replace(/^\/agent-wechat/, '');
  else {
    // Starlight pages are folders: `setup/install.mdx` is served at `/setup/install/`.
    const dir = `/${page.replace(/\.mdx?$/, '').replace(/(^|\/)index$/, '')}/`;
    target = new URL(path, `http://x${dir}`).pathname;
  }
  const slug = target.replace(/^\/|\/$/g, '');
  return { page: slug ? `${slug}.mdx` : 'index.mdx', anchor };
}

/**
 * Compares the docs. `pages` maps each content path (relative to the content folder)
 * to its text. Returns `{ status, problems }`: status per locale lists missing, stale,
 * unstamped and orphan pages; problems are structural errors in up-to-date translations.
 */
export function compareDocs(pages, locales) {
  const english = Object.keys(pages).filter(path => !locales.some(locale => path.startsWith(`${locale}/`)));
  const exists = (page, locale) => (locale ? `${locale}/${page}` in pages : page in pages) || page in pages;
  const status = {};
  const problems = [];
  for (const locale of locales) {
    const entry = { missing: [], stale: [], unstamped: [], orphan: [], current: [] };
    status[locale] = entry;
    for (const page of english) {
      const translated = pages[`${locale}/${page}`];
      if (translated === undefined) {
        entry.missing.push(page);
        continue;
      }
      const { fields, body } = frontmatter(translated);
      const source = pages[page];
      if (!fields.sourceHash) entry.unstamped.push(page);
      else if (fields.sourceHash !== hash(source)) {
        entry.stale.push(page);
        continue;
      } else entry.current.push(page);
      const file = `${docs.content}/${locale}/${page}`;
      const en = frontmatter(source).body;
      const report = message => problems.push({ file, message });
      if (!fields.title) report('frontmatter has no title');
      const enCode = codeBlocks(en);
      const code = codeBlocks(body);
      if (enCode.length !== code.length) report(`has ${code.length} code blocks; English has ${enCode.length}`);
      else
        enCode.forEach((block, i) => {
          if (block.info !== code[i].info) report(`code block ${i + 1} is \`${code[i].info}\`; English is \`${block.info}\``);
          // Diagrams and plain-text blocks are prose and may be translated; code only in comments and strings.
          else if (!proseBlock(block.info) && codeSkeleton(block.code) !== codeSkeleton(code[i].code))
            report(`code block ${i + 1} differs from English beyond comments and quoted text`);
        });
      if (imports(en).join('\n') !== imports(body).join('\n')) report('imports differ from English');
      const enComponents = components(en);
      const used = components(body);
      const missingComponents = enComponents.filter(name => !used.includes(name));
      const extraComponents = used.filter(name => !enComponents.includes(name));
      if (missingComponents.length || extraComponents.length)
        report(`components differ from English (${[...missingComponents.map(n => `missing <${n}>`), ...extraComponents.map(n => `extra <${n}>`)].join(', ')})`);
      for (const url of links(body)) {
        const target = resolveLink(url.replace(new RegExp(`^/(agent-wechat/)?${locale}/`), '/'), page);
        if (!target) continue;
        if (!exists(target.page, locale)) {
          report(`link ${url} points to a page that does not exist`);
          continue;
        }
        if (!target.anchor) continue;
        const targetText = pages[`${locale}/${target.page}`] ?? pages[target.page];
        if (!headingIds(frontmatter(targetText).body).includes(target.anchor)) report(`link ${url}: no heading with ID #${target.anchor}`);
      }
    }
    for (const path of Object.keys(pages).filter(path => path.startsWith(`${locale}/`))) {
      const page = path.slice(locale.length + 1);
      if (!(page in pages)) {
        entry.orphan.push(page);
        problems.push({ file: `${docs.content}/${path}`, message: 'has no English page (remove it)' });
      }
    }
  }
  return { status, problems };
}
