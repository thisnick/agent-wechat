/**
 * Remark plugin for explicit heading IDs: `## 联系人 \{#contacts}` renders the heading
 * "联系人" with the ID `contacts`. Translated pages use it to keep the English IDs,
 * so links and anchors are the same in every language. The backslash stops MDX
 * reading the braces as a JavaScript expression.
 */
export function remarkHeadingIds() {
  return () => (tree) => {
    visit(tree, (node) => {
      if (node.type !== 'heading') return;
      const last = node.children?.at(-1);
      if (last?.type !== 'text') return;
      const match = last.value.match(/\s*\{#([A-Za-z0-9_-]+)\}\s*$/);
      if (!match) return;
      last.value = last.value.slice(0, match.index);
      node.data ??= {};
      node.data.hProperties = { ...node.data.hProperties, id: match[1] };
    });
  };
}

function visit(tree, fn) {
  fn(tree);
  for (const child of tree.children ?? []) visit(child, fn);
}
