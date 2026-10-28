// Inline composer acceptance in a real WebView, using synthetic group data only.
import assert from "node:assert/strict";
import { reloadPage } from "./webdriver-navigation.mjs";

export async function mentionsAcceptance({ script, until, key, resize, sentTexts, memberQueries, capture }) {
  const editor = "document.querySelector('.mention-editor')";
  const click = expression => script(`${expression}.click();return true`);
  const choice = name => `[...document.querySelectorAll('.mention-option')].find(button=>button.querySelector('strong')?.textContent===${JSON.stringify(name)})`;
  const text = async (value, caret = value.length) => {
    await script(`const root=${editor};root.replaceChildren(document.createTextNode(${JSON.stringify(value)}));root.focus();const range=document.createRange();range.setStart(root.firstChild,${caret});range.collapse(true);const selection=getSelection();selection.removeAllRanges();selection.addRange(range);root.dispatchEvent(new InputEvent('input',{bubbles:true,inputType:'insertText'}));return true;`);
  };
  const keyboard = options => script(`const event=new KeyboardEvent('keydown',{bubbles:true,cancelable:true,...${JSON.stringify(options)}});${editor}.dispatchEvent(event);return event.defaultPrevented`);
  const composition = kind => script(`${editor}.dispatchEvent(new CompositionEvent(${JSON.stringify(kind)},{bubbles:true,data:'候选'}));return true`);
  const tokens = () => script(`return [...${editor}.querySelectorAll('[data-mention-id]')].map(node=>({id:node.dataset.mentionId,label:node.dataset.mentionLabel}))`);
  const plain = () => script(`return [...${editor}.childNodes].map(node=>node.nodeType===3?node.textContent:'@'+node.dataset.mentionLabel+'\u2005').join('')`);
  await resize(1120, 820);
  await click("document.querySelector('[data-thread-id=\"room@chatroom\"] .chat-row')");
  await until(`return Boolean(${editor})`, Boolean);
  assert.equal(await script(`return document.activeElement===${editor}`), true);
  const queries = memberQueries.length, sends = sentTexts.length;
  assert.equal(await script("return Boolean(document.querySelector('.mention-picker'))"), false);
  assert.equal(memberQueries.length, queries, "opening a group never fetches the mention roster");

  await text("Before @Ji after", 10);
  await until(`return Boolean(${choice("Jin")})`, Boolean);
  await composition("compositionstart");
  assert.equal(await keyboard({ key: "Enter", keyCode: 13 }), false);
  await composition("compositionend");
  assert.equal(await keyboard({ key: "Enter", keyCode: 229 }), false);
  assert.equal((await tokens()).length, 0);
  assert.equal(sentTexts.length, sends);
  assert.equal(await keyboard({ key: "Enter", keyCode: 13 }), true);
  await until(`return ${editor}.querySelectorAll('[data-mention-id]').length`, count => count === 1);
  assert.deepEqual(await tokens(), [{ id: "wxid_jin", label: "Jin" }]);
  assert.equal(await plain(), "Before @Jin\u2005 after");
  assert.equal(await script("return Boolean(document.querySelector('.mention-picker'))"), false);

  await click("document.querySelector('button[aria-label=\"Remove mention of Jin\"]')");
  assert.deepEqual(await tokens(), []);
  assert.equal(await plain(), "Before  after", "× removes the whole mention, keeping surrounding text");
  await keyboard({ key: "z", ctrlKey: true });
  assert.deepEqual(await tokens(), [{ id: "wxid_jin", label: "Jin" }], "undo restores text and recipient identity together");
  await script("document.querySelector('button[aria-label=\"Remove mention of Jin\"]').focus();return true");
  await key("\uE007");
  await until(`return ${editor}.querySelectorAll('[data-mention-id]').length`, count => count === 0);
  assert.equal(sentTexts.length, sends, "keyboard activation of × cannot send");

  await script(`const root=${editor};root.innerHTML='one<div>two</div>';root.focus();const range=document.createRange();range.setStart(root.lastChild.firstChild,0);range.collapse(true);getSelection().removeAllRanges();getSelection().addRange(range);root.dispatchEvent(new InputEvent('input',{bubbles:true}));return true`);
  await click("document.querySelector('button[aria-label=\"Mention someone\"]')");
  await until(`return Boolean(${choice("Jin")})`, Boolean);
  assert.equal(await script("return document.activeElement===document.querySelector('input[aria-label=\"Search group members\"]')"), true);
  await click(choice("Jin"));
  assert.equal(await script(`return ${editor}.textContent.replace('×','')`), "one@Jin\u2005two");
  assert.equal((await tokens())[0].id, "wxid_jin", "native block line breaks keep the insertion before the intended word");

  await text("😀 Hello  later", 9);
  await click("document.querySelector('button[aria-label=\"Mention someone\"]')");
  await until(`return Boolean(${choice("Mei")})`, Boolean);
  await script(`${choice("Mei")}.focus();return true`);
  await key("\uE00C");
  await until("return Boolean(document.querySelector('.mention-picker'))", value => !value);
  assert.equal(await script(`return document.activeElement===${editor}`), true, "Escape from an option restores composer focus");
  await click("document.querySelector('button[aria-label=\"Mention someone\"]')");
  await until(`return Boolean(${choice("Mei")})`, Boolean);
  await click(choice("Mei"));
  assert.equal(await plain(), "😀 Hello @Mei\u2005 later");
  await keyboard({ key: "Backspace" });
  assert.deepEqual(await tokens(), [], "Backspace removes a whole inline token");
  await keyboard({ key: "z", ctrlKey: true });
  assert.deepEqual(await tokens(), [{ id: "wxid_mei", label: "Mei" }]);
  await click("document.querySelector('button[aria-label=\"Mention someone\"]')");
  await until(`return Boolean(${choice("Jin")})`, Boolean);
  assert.equal(await script(`return Boolean(${choice("Everyone")})`), false);
  await click(choice("Jin"));
  assert.equal((await tokens()).length, 2);
  await capture?.("mentions");
  // Switching chats must restore the same inline recipients instead of plain @text.
  await click("document.querySelector('[data-thread-id=\"open\"] .chat-row')");
  await until("return Boolean(document.querySelector('textarea[aria-label=Message]'))", Boolean);
  await click("document.querySelector('[data-thread-id=\"room@chatroom\"] .chat-row')");
  await until(`return ${editor}?.querySelectorAll('[data-mention-id]').length`, count => count === 2);
  const savedText = await plain();
  await until(`return new Promise((resolve,reject)=>{const open=indexedDB.open('agent-wechat-cache');open.onerror=()=>reject(open.error);open.onsuccess=()=>{const db=open.result,tx=db.transaction('drafts','readonly'),read=tx.objectStore('drafts').getAll();read.onsuccess=()=>resolve(read.result.some(row=>row.key[2]==='pin-fixture' && row.key[3]==='room@chatroom' && row.data.mentionTokens?.length===2));read.onerror=()=>reject(read.error);tx.oncomplete=()=>db.close();}})`, Boolean);
  await reloadPage(script, until);
  await until(`return ${editor}?.querySelectorAll('[data-mention-id]').length`, count => count === 2);
  assert.equal(await plain(), savedText);
  await keyboard({ key: "Enter", keyCode: 13 });
  await until(`return ${editor}?.querySelectorAll('[data-mention-id]').length`, count => count === 2);
  assert.equal(sentTexts.length, sends + 1);
  const sent = sentTexts.at(-1);
  assert.equal(sent.text, savedText);
  assert.deepEqual(sent.mentions.userIds, ["wxid_mei", "wxid_jin"]);
  for (const position of sent.mentions.positions) {
    const name = position.index === 0 ? "Mei" : "Jin";
    assert.equal(sent.text.slice(position.start, position.end), `@${name}\u2005`);
  }
  assert.equal(await script("return document.querySelectorAll('article[data-send-state]').length"), 0, "definitive rejection restores the draft, not an uncertain receipt");
  console.log("Inline mentions: lazy shared-name picker, middle/emoji placement, IME Enter, ×/keyboard removal, atomic Backspace/undo, draft switching/reload and exact send positions passed");
}
