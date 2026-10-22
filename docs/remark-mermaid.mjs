import { renderMermaidSVG } from 'beautiful-mermaid';

/**
 * Renders ```mermaid code blocks to inline SVG at build time.
 * Colors are Starlight CSS variables, so diagrams follow the light/dark theme.
 * The renderer's own <style> (a Google Fonts import and unscoped rules) is
 * replaced by the `.diagram` rules in src/styles/custom.css.
 */
export function remarkMermaid() {
  let count = 0;
  return () => (tree) => {
    visit(tree, (node, index, parent) => {
      if (node.type !== 'code' || node.lang !== 'mermaid' || !parent) return;
      const id = `d${++count}`;
      const svg = renderMermaidSVG(node.value, {
        bg: 'var(--sl-color-black)',
        fg: 'var(--sl-color-white)',
        accent: 'var(--sl-color-text-accent)',
        transparent: true,
        padding: 16,
      })
        .replace(/<style>[\s\S]*?<\/style>/, '')
        .replace(/id="(arrowhead[\w-]*)"/g, `id="${id}-$1"`)
        .replace(/url\(#(arrowhead[\w-]*)\)/g, `url(#${id}-$1)`)
        .replace('<svg ', '<svg role="img" ');
      parent.children[index] = { type: 'html', value: `<figure class="diagram">${svg}</figure>` };
    });
  };
}

function visit(node, fn, index, parent) {
  fn(node, index, parent);
  node.children?.forEach((child, i) => visit(child, fn, i, node));
}
