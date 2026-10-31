import satori from 'satori';
import sharp from 'sharp';
import { readFileSync, readdirSync, statSync, mkdirSync } from 'fs';
import { fileURLToPath } from 'url';
import { resolve, dirname } from 'path';

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
const fontCJK = findFont([
  '/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc',
  '/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc',
  '/usr/share/fonts/truetype/wqy/wqy-microhei.ttc',
  '/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc',
  '/Library/Fonts/Arial Unicode.ttf',
  '/System/Library/Fonts/Supplemental/Arial Unicode.ttf',
  '/System/Library/Fonts/Hiragino Sans GB.ttc',
]);

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
                    fontFamily: 'DejaVu Sans',
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
                          fontFamily: 'DejaVu Sans',
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
                          fontFamily: 'DejaVu Sans',
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
                    fontFamily: 'DejaVu Sans Mono',
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
                    fontFamily: 'DejaVu Sans Mono',
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

async function renderOgImage(title, description, isRoot = false) {
  const svg = await satori(ogTemplate(title, description, isRoot), {
    width: OG_WIDTH,
    height: OG_HEIGHT,
    fonts: [
      { name: 'DejaVu Sans', data: fontRegular, weight: 400, style: 'normal' },
      { name: 'DejaVu Sans', data: fontBold, weight: 700, style: 'normal' },
      { name: 'DejaVu Sans Mono', data: fontMono, weight: 400, style: 'normal' },
      ...(fontCJK ? [{ name: 'DejaVu Sans', data: fontCJK, weight: 400, style: 'normal' }] : []),
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
            const png = await renderOgImage(title, description, slug === 'index' || slug === 'zh-cn');
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
