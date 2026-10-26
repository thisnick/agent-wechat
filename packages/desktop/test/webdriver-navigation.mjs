import { randomUUID } from "node:crypto";

export async function reloadPage(script, until) {
  const marker = JSON.stringify(randomUUID());
  // A WebDriver script returns before reload replaces the document. Mark the
  // outgoing window so readiness checks cannot succeed against its old DOM.
  await script(`window.__acceptanceReload = ${marker};
    window.location.reload(); return true`);
  await until(
    `return window.__acceptanceReload !== ${marker} && document.readyState === 'complete'`,
    Boolean,
  );
}
