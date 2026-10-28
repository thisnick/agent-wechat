import { MENTION_SEPARATOR, type MentionToken } from "./mentions.ts";

export type EditorToken = MentionToken & { node: HTMLElement };
export function readEditor(root: Node): { text: string; tokens: EditorToken[] } {
  let text = "";
  const tokens: EditorToken[] = [];
  function visit(node: Node) {
    if (node.nodeType === 3) { text += node.textContent; return; }
    if (node.nodeType !== 1 && node.nodeType !== 11) return;
    const element = node as HTMLElement;
    if (node.nodeType === 1 && element.hasAttribute("data-mention-id")) {
      const start = text.length, label = element.dataset.mentionLabel!;
      text += `@${label}${MENTION_SEPARATOR}`;
      tokens.push({ userId: element.dataset.mentionId === "" ? null : element.dataset.mentionId!, label, start, end: text.length, node: element });
      return;
    }
    if (node.nodeName === "BR") { text += "\n"; return; }
    if (["DIV", "P"].includes(node.nodeName) && node !== root && text && !text.endsWith("\n")) text += "\n";
    for (const child of node.childNodes) visit(child);
  }
  for (const child of root.childNodes) visit(child);
  return { text, tokens };
}
export function editorDraft(root: Node) {
  const value = readEditor(root);
  return { text: value.text, tokens: value.tokens.map(({ node: _node, ...token }) => token) };
}
export function editorSelection(root: HTMLElement) {
  const selection = window.getSelection();
  if (!selection?.rangeCount) return null;
  const range = selection.getRangeAt(0);
  if (!root.contains(range.startContainer) || !root.contains(range.endContainer)) return null;
  const before = range.cloneRange(); before.selectNodeContents(root); before.setEnd(range.startContainer, range.startOffset);
  const through = range.cloneRange(); through.selectNodeContents(root); through.setEnd(range.endContainer, range.endOffset);
  return { start: readEditor(before.cloneContents()).text.length, end: readEditor(through.cloneContents()).text.length };
}
function point(root: HTMLElement, offset: number): [Node, number] {
  let at = 0, last = "", result: [Node, number] | undefined;
  function walk(node: Node) {
    if (result) return;
    if (node.nodeType === 3) {
      const length = node.textContent?.length ?? 0;
      if (offset <= at + length) result = [node, Math.max(0, offset - at)];
      if (length) last = node.textContent!.slice(-1);
      at += length; return;
    }
    if (node.nodeType !== 1) return;
    const element = node as HTMLElement;
    if (element.hasAttribute("data-mention-id") || node.nodeName === "BR") {
      const length = node.nodeName === "BR" ? 1 : `@${element.dataset.mentionLabel}${MENTION_SEPARATOR}`.length;
      const index = Array.prototype.indexOf.call(node.parentNode!.childNodes, node);
      if (offset <= at + length) result = [node.parentNode!, index + (offset > at ? 1 : 0)];
      last = node.nodeName === "BR" ? "\n" : MENTION_SEPARATOR;
      at += length; return;
    }
    if (["DIV", "P"].includes(node.nodeName) && at && last !== "\n") {
      // Native editing/IME may leave blocks rather than literal line breaks.
      // Count the same separator that readEditor serializes before this block.
      if (offset <= at + 1) {
        const index = Array.prototype.indexOf.call(node.parentNode!.childNodes, node);
        result = offset <= at ? [node.parentNode!, index] : [node, 0];
      }
      at++; last = "\n";
    }
    for (const child of node.childNodes) walk(child);
  }
  for (const child of root.childNodes) walk(child);
  return result ?? [root, root.childNodes.length];
}
export function editorRange(root: HTMLElement, start: number, end = start) {
  const range = document.createRange(); range.setStart(...point(root, start)); range.setEnd(...point(root, end)); return range;
}
export function focusEditor(root: HTMLElement, offset: number) {
  root.focus({ preventScroll: true });
  const range = editorRange(root, offset), selection = window.getSelection()!;
  selection.removeAllRanges(); selection.addRange(range);
}
export function mentionNode(token: MentionToken, remove: (node: HTMLElement) => void) {
  const node = document.createElement("span");
  node.className = "inline-mention"; node.contentEditable = "false";
  node.dataset.mentionId = token.userId ?? ""; node.dataset.mentionLabel = token.label;
  const name = document.createElement("span"); name.textContent = `@${token.label}${MENTION_SEPARATOR}`;
  const button = document.createElement("button"); button.type = "button"; button.className = "inline-mention-remove";
  button.setAttribute("aria-label", `Remove mention of ${token.label}`); button.textContent = "×";
  button.onpointerdown = event => event.preventDefault();
  button.onclick = event => { event.preventDefault(); event.stopPropagation(); remove(node); };
  node.append(name, button); return node;
}
export function restoreEditor(root: HTMLElement, text: string, tokens: MentionToken[], remove: (node: HTMLElement) => void) {
  root.replaceChildren(); let offset = 0;
  for (const token of tokens) { root.append(document.createTextNode(text.slice(offset, token.start)), mentionNode(token, remove)); offset = token.end; }
  root.append(document.createTextNode(text.slice(offset)));
}
export function insertMention(root: HTMLElement, start: number, end: number, userId: string | null, label: string, remove: (node: HTMLElement) => void) {
  const range = editorRange(root, start, end); range.deleteContents();
  const token = { userId, label, start, end: start + label.length + 2 };
  range.insertNode(mentionNode(token, remove)); root.normalize(); focusEditor(root, token.end);
}
export function insertEditorText(root: HTMLElement, start: number, end: number, text: string) {
  const range = editorRange(root, start, end); range.deleteContents(); range.insertNode(document.createTextNode(text)); root.normalize(); focusEditor(root, start + text.length);
}
