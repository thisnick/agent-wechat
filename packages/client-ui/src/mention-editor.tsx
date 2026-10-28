import { useEffect, useId, useLayoutEffect, useRef, useState, type RefObject } from "react";
import type { NativeGroupMember } from "@agent-wechat/shared";
import { isNativeTombstone } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { HubAvatar } from "./avatars.tsx";
import { Icon } from "./ui.tsx";
import { readableError } from "./session.ts";
import { mentionQuery, validMentionTokens, type MentionToken } from "./mentions.ts";
import { editorDraft, editorSelection, focusEditor, insertEditorText, insertMention, readEditor, restoreEditor } from "./mention-editor-dom.ts";

type Draft = { text: string; tokens: MentionToken[] };
type Choice = { id: string | null; label: string; secondary?: string };
export function MentionEditor({ connection, account, thread, inputRef, value, tokens, disabled, allowMentions, openRequest, placeholder, onChange, onSend, onFocus, onEscape }: {
  connection: Connection;
  account: string;
  thread: string;
  inputRef: RefObject<HTMLDivElement | null>;
  value: string;
  tokens: MentionToken[];
  disabled: boolean;
  allowMentions: boolean;
  openRequest: number;
  placeholder: string;
  onChange: (text: string, tokens: MentionToken[]) => void;
  onSend: () => void;
  onFocus: () => void;
  onEscape: () => void;
}) {
  const props = useRef({ onChange, onSend, onFocus, onEscape, disabled, allowMentions });
  props.current = { onChange, onSend, onFocus, onEscape, disabled, allowMentions };
  const [picker, setPicker] = useState(false), [query, setQuery] = useState("");
  const [members, setMembers] = useState<NativeGroupMember[]>([]), [next, setNext] = useState<string | null>(null);
  const [loading, setLoading] = useState(false), [error, setError] = useState("");
  const [loadedQuery, setLoadedQuery] = useState<string | null>(null);
  const [active, setActive] = useState(0), [retry, setRetry] = useState(0);
  const [announcement, announce] = useState("");
  const search = useRef<HTMLInputElement>(null), popup = useRef<HTMLDivElement>(null);
  const focusSearch = useRef(false);
  const composing = useRef(false), selection = useRef({ start: 0, end: 0 });
  const queryRange = useRef<{ start: number; end: number } | null>(null);
  const dismissed = useRef<number | null>(null), emitted = useRef("");
  const history = useRef<{ values: Draft[]; index: number }>({ values: [], index: -1 });
  const listId = useId();
  const firstOpen = useRef(openRequest), request = useRef(0), lifetime = useRef<AbortController | null>(null);
  const hasAll = tokens.some(token => token.userId === null);
  const choices: Choice[] = hasAll || loadedQuery !== query ? [] : members.map(member => ({
    id: member.username,
    // A private contact remark may help identify a person, but is never sent.
    label: member.mentionName || member.roomDisplayName || member.username,
    secondary: member.displayName && member.displayName !== (member.mentionName || member.roomDisplayName || member.username) ? member.displayName : undefined,
  }));
  if (loadedQuery === query && !tokens.length && (!query || "everyone all".includes(query.toLocaleLowerCase()))) choices.push({ id: null, label: "all", secondary: "Everyone in this group" });
  const current = choices[Math.min(active, Math.max(0, choices.length - 1))];
  function close() {
    setPicker(false); setLoadedQuery(null); queryRange.current = null;
    lifetime.current?.abort(); request.current++;
  }
  function record(draft: Draft) {
    const stack = history.current;
    if (JSON.stringify(stack.values[stack.index]) === JSON.stringify(draft)) return;
    stack.values = [...stack.values.slice(0, stack.index + 1), draft].slice(-100); stack.index = stack.values.length - 1;
  }
  function emit(commit = true) {
    const root = inputRef.current;
    if (!root) return;
    const draft = editorDraft(root);
    if (!validMentionTokens(draft.text, draft.tokens)) { setError("Choose the mentioned members again."); return; }
    emitted.current = JSON.stringify(draft);
    if (commit) record(draft);
    selection.current = editorSelection(root) ?? selection.current;
    props.current.onChange(draft.text, draft.tokens);
  }
  function remove(node: HTMLElement) {
    const root = inputRef.current;
    if (!root || props.current.disabled) return;
    const token = readEditor(root).tokens.find(token => token.node === node);
    if (!token) return;
    node.remove(); root.normalize(); focusEditor(root, token.start);
    setError(""); close(); emit(); announce(`Removed mention of ${token.label}`);
  }
  useLayoutEffect(() => {
    const root = inputRef.current;
    if (!root || composing.current) return;
    const draft = { text: value, tokens }, key = JSON.stringify(draft);
    if (key !== JSON.stringify(editorDraft(root))) {
      const focused = document.activeElement === root;
      const at = key !== emitted.current ? value.length : editorSelection(root)?.end ?? value.length;
      restoreEditor(root, value, tokens, remove);
      selection.current = { start: Math.min(at, value.length), end: Math.min(at, value.length) };
      if (focused) focusEditor(root, Math.min(at, value.length));
    }
    if (key !== emitted.current) { emitted.current = key; history.current = { values: [draft], index: 0 }; }
  }, [value, tokens]);
  useEffect(() => {
    const changed = () => { const root = inputRef.current; if (root) selection.current = editorSelection(root) ?? selection.current; };
    document.addEventListener("selectionchange", changed);
    return () => document.removeEventListener("selectionchange", changed);
  }, []);
  function undo(redo: boolean) {
    const root = inputRef.current, stack = history.current, index = stack.index + (redo ? 1 : -1);
    if (!root || index < 0 || index >= stack.values.length) return;
    stack.index = index;
    const draft = stack.values[index]; restoreEditor(root, draft.text, draft.tokens, remove); focusEditor(root, draft.text.length);
    emitted.current = JSON.stringify(draft); props.current.onChange(draft.text, draft.tokens); close();
  }
  function boundary(event: { preventDefault(): void }, backward: boolean) {
    const root = inputRef.current;
    if (!root || composing.current || props.current.disabled) return false;
    const at = editorSelection(root);
    if (!at || at.start !== at.end) return false;
    const token = readEditor(root).tokens.find(token => backward ? token.end === at.start : token.start === at.start);
    if (!token) return false;
    event.preventDefault(); remove(token.node); return true;
  }
  useEffect(() => {
    const root = inputRef.current;
    if (!root) return;
    const before = (event: InputEvent) => {
      if (event.isComposing || composing.current) return;
      if (event.inputType === "deleteContentBackward") boundary(event, true);
      if (event.inputType === "deleteContentForward") boundary(event, false);
      if (["historyUndo", "historyRedo"].includes(event.inputType)) { event.preventDefault(); undo(event.inputType === "historyRedo"); }
      if (["insertParagraph", "insertLineBreak"].includes(event.inputType)) {
        event.preventDefault();
        const at = editorSelection(root) ?? selection.current;
        insertEditorText(root, at.start, at.end, "\n"); emit();
      }
    };
    root.addEventListener("beforeinput", before);
    return () => root.removeEventListener("beforeinput", before);
  }, []);
  function open(searchText = "", fromButton = false) {
    if (!props.current.allowMentions || props.current.disabled) return;
    setError(""); setQuery(searchText); setActive(0); setPicker(true);
    if (fromButton) { focusSearch.current = true; search.current?.focus({ preventScroll: true }); }
  }
  useLayoutEffect(() => {
    if (picker && focusSearch.current && search.current) {
      focusSearch.current = false; search.current.focus({ preventScroll: true });
    }
  }, [picker, openRequest]);
  useEffect(() => {
    if (firstOpen.current === openRequest) return;
    firstOpen.current = openRequest; queryRange.current = null; open("", true);
  }, [openRequest]);
  useEffect(() => { if (!allowMentions || disabled) close(); }, [allowMentions, disabled]);
  function detect() {
    const root = inputRef.current;
    if (!root || composing.current || !props.current.allowMentions) return;
    const at = editorSelection(root), draft = editorDraft(root);
    if (!at || at.start !== at.end) return;
    const match = mentionQuery(draft.text, at.start, draft.tokens);
    if (!match) { if (queryRange.current) close(); dismissed.current = null; return; }
    if (match.start === dismissed.current) return;
    queryRange.current = match; open(match.query);
  }
  useEffect(() => {
    if (!picker) return;
    const controller = new AbortController(), number = ++request.current;
    lifetime.current?.abort(); lifetime.current = controller;
    setMembers([]); setNext(null); setLoadedQuery(null); setLoading(true); setError("");
    const timer = setTimeout(() => {
      void connection.session.client.groupMembers(account, thread, { limit: 50, query }, controller.signal).then(page => {
        if (controller.signal.aborted || request.current !== number) return;
        setMembers(page.items.filter((member): member is NativeGroupMember => !isNativeTombstone(member))); setNext(page.nextCursor); setLoadedQuery(query);
      }).catch(error => { if (!controller.signal.aborted && request.current === number) setError(readableError(error)); })
        .finally(() => { if (!controller.signal.aborted && request.current === number) setLoading(false); });
    }, 120);
    return () => { clearTimeout(timer); controller.abort(); };
  }, [connection, account, thread, picker, query, retry]);
  async function more() {
    if (!next || loading) return;
    const number = request.current, signal = lifetime.current?.signal;
    setLoading(true);
    try {
      const page = await connection.session.client.groupMembers(account, thread, { limit: 50, query, cursor: next }, signal);
      if (signal?.aborted || request.current !== number) return;
      setMembers(values => [...new Map([...values, ...page.items.filter((member): member is NativeGroupMember => !isNativeTombstone(member))].map(member => [member.username, member])).values()]); setNext(page.nextCursor);
    } catch (error) { if (!signal?.aborted && request.current === number) setError(readableError(error)); }
    finally { if (!signal?.aborted && request.current === number) setLoading(false); }
  }
  useEffect(() => {
    if (!picker) return;
    const outside = (event: PointerEvent) => {
      if (!popup.current?.contains(event.target as Node) && !inputRef.current?.contains(event.target as Node)) close();
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [picker]);
  useEffect(() => {
    if (!picker) return;
    const list = popup.current?.querySelector<HTMLElement>(".mention-options");
    const item = list?.querySelector<HTMLElement>('[aria-selected="true"]');
    if (!list || !item) return;
    const top = item.getBoundingClientRect().top - list.getBoundingClientRect().top + list.scrollTop;
    if (top < list.scrollTop) list.scrollTop = top;
    else if (top + item.offsetHeight > list.scrollTop + list.clientHeight)
      list.scrollTop = top + item.offsetHeight - list.clientHeight;
  }, [active, members, picker]);
  function choose(choice: Choice) {
    const root = inputRef.current;
    if (!root || props.current.disabled || !props.current.allowMentions) return;
    const draft = readEditor(root), at = queryRange.current ?? selection.current;
    if (draft.tokens.length >= 256 || (choice.id !== null && !draft.tokens.some(token => token.userId === choice.id) && new Set(draft.tokens.map(token => token.userId)).size >= 128)) { setError("Too many mentions in this message."); return; }
    if ((choice.id === null && draft.tokens.length) || draft.tokens.some(token => token.userId === null)) { setError("Remove existing mentions before choosing Everyone."); return; }
    insertMention(root, at.start, at.end, choice.id, choice.label, remove);
    close(); dismissed.current = null; emit(); announce(`Mentioning ${choice.label}`);
  }
  function dismiss() {
    dismissed.current = queryRange.current?.start ?? null; close();
    if (inputRef.current) focusEditor(inputRef.current, selection.current.end);
  }
  function keyboard(event: React.KeyboardEvent<HTMLElement>) {
    if (event.target instanceof Element && event.target.closest("button")) return;
    if (event.defaultPrevented || composing.current || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
    if ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === "z") { event.preventDefault(); undo(event.shiftKey); return; }
    if (picker) {
      if (["ArrowDown", "ArrowUp"].includes(event.key)) { event.preventDefault(); if (choices.length) setActive(index => (index + (event.key === "ArrowDown" ? 1 : -1) + choices.length) % choices.length); return; }
      if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); if (!event.repeat && current) choose(current); return; }
      if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); dismiss(); return; }
    }
    if (event.currentTarget === search.current) return;
    if (event.key === "Escape") { props.current.onEscape(); return; }
    if (event.key === "Enter") {
      event.preventDefault(); if (event.repeat) return;
      if (event.shiftKey) { const root = inputRef.current!, at = editorSelection(root) ?? selection.current; insertEditorText(root, at.start, at.end, "\n"); emit(); }
      else props.current.onSend();
    }
    if (event.key === "Backspace") boundary(event, true);
    if (event.key === "Delete") boundary(event, false);
  }
  function copy(event: React.ClipboardEvent, cut: boolean) {
    const selection = window.getSelection(), range = selection?.rangeCount ? selection.getRangeAt(0) : undefined;
    if (!range || !inputRef.current?.contains(range.commonAncestorContainer)) return;
    event.preventDefault(); event.clipboardData.setData("text/plain", readEditor(range.cloneContents()).text);
    if (cut) { range.deleteContents(); emit(); }
  }
  return <>
    <div ref={inputRef} className="mention-editor" contentEditable={!disabled} suppressContentEditableWarning role="textbox" aria-label="Message" aria-multiline="true" aria-disabled={disabled} aria-haspopup="listbox" aria-expanded={picker} aria-controls={listId} aria-activedescendant={picker && current ? `${listId}-${Math.min(active, choices.length - 1)}` : undefined} data-placeholder={placeholder}
      onFocus={onFocus} onClick={onFocus} onInput={() => { emit(!composing.current); detect(); }}
      onCompositionStart={() => { composing.current = true; }} onCompositionEnd={() => { composing.current = false; emit(); detect(); }} onBlur={() => { composing.current = false; }} onKeyDown={keyboard}
      onCopy={event => copy(event, false)} onCut={event => copy(event, true)}
      onDrop={event => { event.preventDefault(); setError("Use the attachment button to add files."); }}
      onPaste={event => { event.preventDefault(); if (composing.current || disabled) return; const text = event.clipboardData.getData("text/plain"); if (new TextEncoder().encode(text).length > 65536) { setError("Message is too long."); return; } const root = inputRef.current!, at = editorSelection(root) ?? selection.current; insertEditorText(root, at.start, at.end, text); emit(); detect(); }} />
    {picker && <div ref={popup} className="mention-picker" onKeyDown={event => {
      if (event.key === "Escape" && !event.defaultPrevented && !composing.current && !event.nativeEvent.isComposing && event.nativeEvent.keyCode !== 229) {
        event.preventDefault(); event.stopPropagation(); dismiss();
      }
    }}>
      <div className="mention-picker-heading"><span>Mention someone</span><button type="button" className="icon-btn" aria-label="Close mentions" onClick={dismiss}><Icon name="close" /></button></div>
      <label className="mention-search"><Icon name="search" /><input ref={search} role="combobox" aria-expanded={picker} aria-controls={listId} aria-activedescendant={current ? `${listId}-${Math.min(active, choices.length - 1)}` : undefined} aria-label="Search group members" value={query} placeholder="Find a group member" onChange={event => { setQuery(event.target.value); setActive(0); }} onKeyDown={keyboard} onCompositionStart={() => { composing.current = true; }} onCompositionEnd={() => { composing.current = false; }} /></label>
      <div id={listId} className="mention-options" role="listbox" aria-label="Group members" aria-busy={loading}>
        {choices.map((choice, index) => <button type="button" role="option" id={`${listId}-${index}`} key={choice.id ?? "@all"} aria-selected={current === choice} className={`mention-option ${current === choice ? "active" : ""} ${choice.id === null ? "all" : ""}`} onPointerDown={event => event.preventDefault()} onPointerEnter={() => setActive(index)} onClick={() => choose(choice)}>
          {choice.id ? <HubAvatar connection={connection} account={account} username={choice.id} name={choice.label} /> : <span className="mention-everyone" aria-hidden="true">@</span>}
          <span><strong>{choice.id === null ? "Everyone" : choice.label}</strong>{choice.secondary && <small>{choice.secondary}</small>}</span>
        </button>)}
        {loading && <span className="mention-loading" role="status"><Icon name="refresh" /><span className="sr-only">Loading group members</span></span>}
        {!loading && !error && !choices.length && <p className="mention-empty">{hasAll ? "Already mentioning everyone" : "No matching members"}</p>}
        {next && <button type="button" className="mention-more" disabled={loading} onClick={() => void more()}>More members</button>}
      </div>
      {error && <p className="mention-error" role="alert">{error} <button type="button" onClick={() => setRetry(value => value + 1)}>Retry</button></p>}
    </div>}
    {!picker && error && <p className="mention-error" role="alert">{error}</p>}
    <span className="sr-only" aria-live="polite">{announcement}</span>
  </>;
}
