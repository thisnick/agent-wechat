// Exercise browser composition event ordering against a synthetic send endpoint.
import assert from "node:assert/strict";

export async function composerAcceptance({ script, until, sentTexts }) {
  const field = "document.querySelector('textarea[aria-label=Message]')";
  const draft = async (text) => {
    await script(`
      const input=${field};input.focus();
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(input,${JSON.stringify(text)});
      input.dispatchEvent(new Event('input',{bubbles:true}));return true;
    `);
    await until(`return ${field}.value`, (value) => value === text);
  };
  const composition = (kind) => script(`
    ${field}.dispatchEvent(new CompositionEvent(${JSON.stringify(kind)},{bubbles:true,data:'候选'}));return true;
  `);
  const key = (options) => script(`
    const input=${field};
    const event=new KeyboardEvent('keydown',{bubbles:true,cancelable:true,...${JSON.stringify(options)}});
    input.dispatchEvent(event);
    return {prevented:event.defaultPrevented,keyCode:event.keyCode};
  `);
  const untouched = async (options, text) => {
    const result = await key(options);
    assert.equal(result.prevented, false, "candidate input retains its native default action");
    assert.equal(await script(`return ${field}.value`), text);
    assert.equal(sentTexts.length, 0, "candidate input never reaches the send endpoint");
    return result;
  };

  await draft("你好");
  await composition("compositionstart");
  await untouched({ key: "Enter", keyCode: 13, isComposing: false }, "你好");
  await composition("compositionend");

  await draft("こんにちは");
  await untouched({ key: "Enter", keyCode: 13, isComposing: true }, "こんにちは");

  await draft("안녕하세요");
  await composition("compositionstart");
  await composition("compositionend");
  const webkit = await untouched({ key: "Enter", keyCode: 229, isComposing: false }, "안녕하세요");
  assert.equal(webkit.keyCode, 229, "the WebKit ordering fixture exposes the IME key code");
  await untouched({ key: "Enter", keyCode: 13, shiftKey: true }, "안녕하세요");
  assert.equal((await key({ key: "Enter", keyCode: 13, repeat: true })).prevented, true);
  assert.equal(sentTexts.length, 0, "a held confirmation key cannot send after composition ends");

  await script("document.querySelector('article[data-message-id=\"open-60\"] button[aria-label=Reply]').click();return true;");
  await until("return Boolean(document.querySelector('.composer-reply'))", Boolean);
  await composition("compositionstart");
  assert.equal((await key({ key: "Escape", keyCode: 27 })).prevented, false);
  assert.equal(await script("return Boolean(document.querySelector('.composer-reply'))"), true);
  await composition("compositionend");
  assert.equal((await key({ key: "Escape", keyCode: 27 })).prevented, true);
  await until("return Boolean(document.querySelector('.composer-reply'))", (value) => !value);

  await draft("Committed text");
  await composition("compositionstart");
  await script(`${field}.blur();${field}.focus();return true;`);
  assert.equal((await key({ key: "Enter", keyCode: 13 })).prevented, true);
  for (let attempt = 0; sentTexts.length === 0 && attempt < 100; attempt++)
    await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(sentTexts.length, 1, "a new ordinary Enter sends exactly once after focus changes");
  assert.equal(sentTexts[0].text, "Committed text");
  console.log("Composer: native/ref composition guards, WebKit end-before-Enter, Shift+Enter, repeat suppression, IME Escape and blur recovery passed");
}
