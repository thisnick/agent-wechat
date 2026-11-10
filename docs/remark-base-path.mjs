/**
 * Remark plugin that prefixes internal absolute links with Astro's base path.
 * Starlight auto-prefixes sidebar links but NOT links written in MDX content.
 *
 * In a translated page (under `src/content/docs/<locale>/`), a link such as
 * `/setup/install/` also gets the locale, so translations keep the English links
 * unchanged and still stay in their own language.
 */
export function remarkBasePath(base, locales = []) {
  const prefix = base.replace(/\/$/, '');

  return () => (tree, file) => {
    const locale = pageLocale(file?.path ?? file?.history?.[0] ?? '', locales);
    visit(tree, 'link', (node) => {
      if (!node.url || !node.url.startsWith('/') || node.url.startsWith('//')) return;
      if (prefix && node.url.startsWith(`${prefix}/`)) return;
      let url = node.url;
      if (locale && !url.startsWith(`/${locale}/`)) url = `/${locale}${url}`;
      node.url = prefix + url;
    });
  };
}

/** The locale folder a content file sits in, if any. */
export function pageLocale(path, locales) {
  const match = path.replace(/\\/g, '/').match(/\/src\/content\/docs\/([^/]+)\//);
  return match && locales.includes(match[1]) ? match[1] : null;
}

function visit(tree, type, fn) {
  if (tree.type === type) fn(tree);
  if (tree.children) {
    for (const child of tree.children) {
      visit(child, type, fn);
    }
  }
}
