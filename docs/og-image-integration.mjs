import satori from 'satori';
import sharp from 'sharp';
import { readFileSync, readdirSync, statSync, mkdirSync } from 'fs';
import { fileURLToPath } from 'url';
import { resolve, dirname } from 'path';
import { translatedLocales } from './locales.mjs';

export const OG_WIDTH = 1200;
export const OG_HEIGHT = 630;

function findFont(candidates) {
  for (const p of candidates) {
    try {
      return readFileSync(p);
    } catch {}
  }
  return null;
}

const fontRegular = findFont([
  '/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf',
  '/System/Library/Fonts/Supplemental/Arial.ttf',
]);
const fontBold = findFont([
  '/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf',
  '/System/Library/Fonts/Supplemental/Arial Bold.ttf',
]);
const fontMono = findFont([
  '/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf',
  '/System/Library/Fonts/Supplemental/Courier New.ttf',
]);
// Chinese, Japanese and Korean titles need a CJK font. Linux packages ship these as
// collections (.ttc), which satori cannot read, so one face is extracted below.
const fontCJKFile = findFont([
  '/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc',
  '/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc',
  '/usr/share/fonts/truetype/wqy/wqy-microhei.ttc',
  '/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc',
  '/Library/Fonts/Arial Unicode.ttf',
  '/System/Library/Fonts/Supplemental/Arial Unicode.ttf',
  '/System/Library/Fonts/Hiragino Sans GB.ttc',
]);

/**
 * One font out of a TrueType/OpenType collection, preferring the face whose family
 * name ends in "SC" (Simplified Chinese), as a standalone font file.
 */
export function collectionFace(buffer) {
  if (buffer.toString('latin1', 0, 4) !== 'ttcf') return buffer;
  const count = buffer.readUInt32BE(8);
  const offsets = Array.from({ length: count }, (_, i) => buffer.readUInt32BE(12 + i * 4));
  const tables = (offset) => Array.from({ length: buffer.readUInt16BE(offset + 4) }, (_, i) => {
    const record = offset + 12 + i * 16;
    return { tag: buffer.toString('latin1', record, record + 4), checksum: buffer.readUInt32BE(record + 4), offset: buffer.readUInt32BE(record + 8), length: buffer.readUInt32BE(record + 12) };
  });
  const family = (offset) => {
    const name = tables(offset).find((table) => table.tag === 'name');
    if (!name) return '';
    const records = buffer.readUInt16BE(name.offset + 2);
    const strings = name.offset + buffer.readUInt16BE(name.offset + 4);
    for (let i = 0; i < records; i++) {
      const record = name.offset + 6 + i * 12;
      if (buffer.readUInt16BE(record) === 3 && buffer.readUInt16BE(record + 6) === 1) {
        const start = strings + buffer.readUInt16BE(record + 10);
        return buffer.subarray(start, start + buffer.readUInt16BE(record + 8)).swap16().toString('utf16le');
      }
    }
    return '';
  };
  const chosen = offsets.find((offset) => / SC$/.test(family(offset))) ?? offsets[0];
  const list = tables(chosen);
  const header = 12 + list.length * 16;
  const sizes = list.map((table) => (table.length + 3) & ~3);
  const out = Buffer.alloc(header + sizes.reduce((sum, size) => sum + size, 0));
  buffer.copy(out, 0, chosen, chosen + 12);
  let at = header;
  list.forEach((table, i) => {
    const record = 12 + i * 16;
    out.write(table.tag, record, 'latin1');
    out.writeUInt32BE(table.checksum, record + 4);
    out.writeUInt32BE(at, record + 8);
    out.writeUInt32BE(table.length, record + 12);
    buffer.copy(out, at, table.offset, table.offset + table.length);
    at += sizes[i];
  });
  return out;
}

let fontCJK;
const cjk = /[\u2E80-\u9FFF\uAC00-\uD7AF\uF900-\uFAFF\uFF00-\uFFEF]/;

function ogTemplate(title, description, isRoot = false) {
  return {
    type: 'div',
    props: {
      style: {
        display: 'flex',
        flexDirection: 'column',
        width: '100%',
        height: '100%',
        backgroundColor: '#111111',
        padding: '0',
      },
      children: [
        // Accent bar
        {
          type: 'div',
          props: {
            style: {
              position: 'absolute',
              left: 0,
              top: 0,
              width: '5px',
              height: '100%',
              backgroundColor: '#bd5721',
            },
          },
        },
        // Content area
        {
          type: 'div',
          props: {
            style: {
              display: 'flex',
              flexDirection: 'column',
              justifyContent: 'center',
              flexGrow: 1,
              padding: '80px',
              paddingLeft: '80px',
            },
            children: [
              // H1: project name (non-root) or page title (root)
              {
                type: 'div',
                props: {
                  style: {
                    fontSize: '64px',
                    fontWeight: 700,
                    color: '#FFFFFF',
                    letterSpacing: '-1.5px',
                    lineHeight: 1.1,
                    marginBottom: isRoot ? '24px' : '12px',
                    fontFamily: 'DejaVu Sans, CJK',
                  },
                  children: isRoot ? title : 'agent-wechat',
                },
              },
              // Non-root: page title as H2
              ...(!isRoot
                ? [
                    {
                      type: 'div',
                      props: {
                        style: {
                          fontSize: '36px',
                          fontWeight: 700,
                          color: '#CCCCCC',
                          lineHeight: 1.2,
                          marginBottom: '16px',
                          fontFamily: 'DejaVu Sans, CJK',
                        },
                        children: title,
                      },
                    },
                  ]
                : []),
              // Description
              ...(description
                ? [
                    {
                      type: 'div',
                      props: {
                        style: {
                          fontSize: '22px',
                          color: '#888888',
                          lineHeight: 1.4,
                          fontFamily: 'DejaVu Sans, CJK',
                          maxWidth: '800px',
                        },
                        children: description,
                      },
                    },
                  ]
                : []),
            ],
          },
        },
        // Footer
        {
          type: 'div',
          props: {
            style: {
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'space-between',
              padding: '0 80px 48px 80px',
            },
            children: [
              {
                type: 'div',
                props: {
                  style: {
                    fontSize: '18px',
                    color: '#444444',
                    fontFamily: 'DejaVu Sans Mono, CJK',
                  },
                  children: 'agent-wechat',
                },
              },
              {
                type: 'div',
                props: {
                  style: {
                    fontSize: '16px',
                    color: '#bd5721',
                    fontFamily: 'DejaVu Sans Mono, CJK',
                  },
                  children: 'docs',
                },
              },
            ],
          },
        },
      ],
    },
  };
}

function parseFrontmatter(content) {
  const match = content.match(/^---\n([\s\S]*?)\n---/);
  if (!match) return {};
  const fm = {};
  for (const line of match[1].split('\n')) {
    const colon = line.indexOf(':');
    if (colon === -1) continue;
    const key = line.slice(0, colon).trim();
    const val = line.slice(colon + 1).trim();
    if (key && val) fm[key] = val;
  }
  return fm;
}

/** The CJK font, loaded on first use, only for images whose text needs it. */
function cjkFonts(text) {
  if (!fontCJKFile || !cjk.test(text)) return [];
  fontCJK ??= collectionFace(fontCJKFile);
  // A family of its own: satori falls back to it only for glyphs DejaVu lacks.
  return [400, 700].map((weight) => ({ name: 'CJK', data: fontCJK, weight, style: 'normal' }));
}

async function renderOgImage(title, description, isRoot = false) {
  const svg = await satori(ogTemplate(title, description, isRoot), {
    width: OG_WIDTH,
    height: OG_HEIGHT,
    fonts: [
      { name: 'DejaVu Sans', data: fontRegular, weight: 400, style: 'normal' },
      { name: 'DejaVu Sans', data: fontBold, weight: 700, style: 'normal' },
      { name: 'DejaVu Sans Mono', data: fontMono, weight: 400, style: 'normal' },
      ...cjkFonts(`${title} ${description}`),
    ],
  });
  return sharp(Buffer.from(svg)).png().toBuffer();
}

/**
 * Astro integration that generates per-page OG images at build/dev start.
 * Each doc page gets its own PNG based on its title and description.
 * A fallback og-image.png is also generated for the index page.
 */
export function ogImage() {
  return {
    name: 'og-image',
    hooks: {
      'astro:config:setup': async ({ config }) => {
        if (!fontRegular || !fontBold || !fontMono) {
          console.warn('[og-image] Skipping OG image generation: required fonts not found');
          return;
        }

        const dir = fileURLToPath(config.root);
        const contentDir = resolve(dir, 'src/content/docs');
        const outDir = resolve(dir, 'public/og');

        mkdirSync(outDir, { recursive: true });

        // Find all doc pages
        const files = readdirSync(contentDir, { recursive: true })
          .filter(f => f.endsWith('.md') || f.endsWith('.mdx'));

        let generated = 0;
        let skipped = 0;

        for (const file of files) {
          const filePath = resolve(contentDir, file);
          const slug = file.replace(/\.(md|mdx)$/, '').replace(/\/index$/, '') || 'index';
          const pngPath = resolve(outDir, `${slug}.png`);

          // Skip if PNG is newer than source
          try {
            const srcStat = statSync(filePath);
            const pngStat = statSync(pngPath);
            if (pngStat.mtimeMs >= srcStat.mtimeMs) {
              skipped++;
              continue;
            }
          } catch {
            // PNG doesn't exist yet
          }

          const content = readFileSync(filePath, 'utf-8');
          const fm = parseFrontmatter(content);
          const title = fm.title || slug;
          const description = fm.description || '';

          mkdirSync(dirname(pngPath), { recursive: true });

          try {
            const png = await renderOgImage(title, description, slug === 'index' || translatedLocales.includes(slug));
            await sharp(png).toFile(pngPath);
            generated++;
          } catch (err) {
            console.error(`[og-image] Failed to generate ${slug}.png:`, err.message);
          }
        }

        // Also generate fallback at public/og-image.png
        const fallbackPath = resolve(dir, 'public/og-image.png');
        try {
          statSync(fallbackPath);
        } catch {
          const png = await renderOgImage(
            'agent-wechat',
            'Programmable WeChat for AI agents.',
            true
          );
          await sharp(png).toFile(fallbackPath);
        }

        console.log(`[og-image] Generated ${generated} images, ${skipped} up to date`);
      },
    },
  };
}
