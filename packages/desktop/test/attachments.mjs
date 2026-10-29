// Native composer acceptance uses synthetic files and an isolated HTTP fixture.
import assert from "node:assert/strict";

export async function attachmentsAcceptance({
  script,
  until,
  uploadedFiles,
  sentMedia,
  sentTexts,
  sendRequests,
  queueNextSend,
  capture,
}) {
  const inline = "document.querySelector('.mention-editor')";
  const plain = "document.querySelector('textarea[aria-label=Message]')";
  const tray = "document.querySelector('[aria-label=\"Staged attachments\"]')";
  const send = "document.querySelector('.send-btn')";
  const draft = "File draft stays here";
  const click = (target) => script(`${target}.click();return true;`);
  const help = (label) =>
    script(
      `const button=document.querySelector('button[aria-label=${JSON.stringify(
        label,
      )}]');button.focus();button.click();return true;`,
    );
  const transfer = (files) => `const data = new DataTransfer();
    for (const name of ${JSON.stringify(
      files,
    )}) data.items.add(new File(['fixture'], name));`;
  const event = (kind, target, files = ["photo.PNG"], text = "") =>
    script(`
    ${transfer(files)}
    if (${JSON.stringify(text)}) data.setData('text/plain', ${JSON.stringify(
      text,
    )});
    const event = new Event(${JSON.stringify(
      kind,
    )}, { bubbles:true, cancelable:true });
    Object.defineProperty(event, ${JSON.stringify(
      kind === "paste" ? "clipboardData" : "dataTransfer",
    )}, { value:data });
    ${target}.dispatchEvent(event);
    return event.defaultPrevented;
  `);
  const staged = (count) =>
    until(
      "return document.querySelectorAll('.composer-attachment').length",
      (value) => value === count,
    );
  const remove = (name) =>
    click(
      `document.querySelector('button[aria-label=${JSON.stringify(
        `Remove ${name}`,
      )}]')`,
    );
  const text = async (value) => {
    await script(
      `${inline}.textContent=${JSON.stringify(
        value,
      )};${inline}.dispatchEvent(new Event('input',{bubbles:true}));${inline}.focus();return true;`,
    );
    await until(`return ${inline}.textContent`, (current) => current === value);
  };
  const mediaCount = async (count) => {
    for (let i = 0; i < 150 && sentMedia.length < count; i++)
      await new Promise((resolve) => setTimeout(resolve, 20));
    assert.equal(
      sentMedia.length,
      count,
      await script("return document.querySelector('main')?.innerText"),
    );
  };
  const tokenIds = () =>
    script(
      `return [...${inline}.querySelectorAll('[data-mention-id]')].map(node => node.dataset.mentionId)`,
    );
  const initialMentions = await tokenIds(),
    initialTexts = sentTexts.length;
  assert.equal(
    initialMentions.length,
    2,
    "the preceding mention journey leaves its rejected draft intact",
  );

  // A rejected mentioned-text admission keeps both recipients and unsent files.
  assert.equal(await event("paste", inline, ["with-mentions.png"]), true);
  await staged(1);
  assert.equal(
    uploadedFiles.length,
    0,
    "pasting alongside mentions never uploads immediately",
  );
  assert.deepEqual(await tokenIds(), initialMentions);
  await click(send);
  await until(
    `return ${inline}?.querySelectorAll('[data-mention-id]').length`,
    (count) => count === 2,
  );
  await until(`return !${send}.disabled`, Boolean);
  assert.equal(sentTexts.length, initialTexts + 1);
  assert.equal(
    uploadedFiles.length,
    0,
    "definitively rejected text cannot release its files to the outbox",
  );
  assert.equal(sentMedia.length, 0);
  assert.deepEqual(await tokenIds(), initialMentions);
  await staged(1);
  await remove("with-mentions.png");
  await staged(0);
  await until(`return document.activeElement===${inline}`, Boolean);
  await text(draft);
  await until(
    `return ${inline}.querySelectorAll('[data-mention-id]').length`,
    (count) => count === 0,
  );

  assert.equal(
    await event("paste", inline, ["photo.PNG"], "clipboard description"),
    true,
  );
  await staged(1);
  assert.equal(
    await script(`return ${inline}.textContent`),
    draft,
    "clipboard text does not clobber the message draft",
  );
  assert.equal(
    await script(
      "return document.querySelectorAll('[role=dialog][aria-label=\"Send attachment\"]').length",
    ),
    0,
  );
  assert.equal(
    await script(`return ${tray}.querySelectorAll('select').length`),
    0,
  );
  await until(
    `return Boolean(${tray}?.querySelector('img[src^="blob:"]'))`,
    Boolean,
  );
  assert.equal(
    uploadedFiles.length,
    0,
    "staging and previewing are entirely local",
  );
  await capture?.("attachment-staging");
  await remove("photo.PNG");
  await staged(0);
  await until(`return document.activeElement===${inline}`, Boolean);
  assert.equal(await script(`return ${inline}.textContent`), draft);

  await event("dragenter", inline);
  await until(
    "return Boolean(document.querySelector('.composer-drop-hint'))",
    Boolean,
  );
  assert.equal(
    await event("drop", inline, ["photo.PNG", "movie.MP4", "notes.pdf"]),
    true,
  );
  await staged(3);
  assert.equal(
    await script(
      "return Boolean(document.querySelector('.composer-drop-hint'))",
    ),
    false,
  );
  assert.equal(uploadedFiles.length, 0);
  assert.equal(sentMedia.length, 0);
  assert.equal(
    await script(
      `return ${tray}.textContent.includes('Each file sends as a separate message.')`,
    ),
    false,
  );

  const quality = `${tray}.querySelector('input[type=checkbox]')`;
  assert.equal(
    await script(`return ${quality}.closest('label').textContent.trim()`),
    "Original media quality",
  );
  assert.equal(
    await script(
      "return document.querySelectorAll('.composer-help [role=tooltip]').length",
    ),
    0,
  );
  await help("About original media quality");
  await until(
    "return document.querySelector('[role=tooltip]')?.textContent",
    (value) => value?.includes("unchanged video file"),
  );
  await script(
    "document.activeElement.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));return true;",
  );
  await until(
    "return document.querySelectorAll('.composer-help [role=tooltip]').length",
    (count) => count === 0,
  );
  await click(quality);
  assert.equal(await script(`return ${quality}.checked`), true);
  await help("Keyboard shortcuts");
  await until(
    "return document.querySelector('[role=tooltip]')?.textContent",
    (value) => value?.includes("Shift") && value.includes("Enter"),
  );
  await script(
    "document.activeElement.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));return true;",
  );
  await until(
    "return document.querySelectorAll('.composer-help [role=tooltip]').length",
    (count) => count === 0,
  );

  const requestStart = sendRequests.length;
  queueNextSend();
  // The same event turn cannot claim a staged batch twice.
  await script(`${send}.click();${send}.click();return true;`);
  await staged(0);
  await mediaCount(3);
  assert.deepEqual(
    uploadedFiles.map((file) => file.name),
    ["photo.PNG", "movie.MP4", "notes.pdf"],
  );
  assert.deepEqual(
    sentMedia.map((item) => item.kind),
    ["image", "video", "file"],
  );
  assert.ok(sentMedia.every((item) => item.threadId === "room@chatroom"));
  assert.equal(sentMedia[1].videoQuality, "original");
  assert.equal(
    sentTexts.length,
    initialTexts + 2,
    "the composer sends its text only once",
  );
  assert.equal(sentTexts.at(-1).text, draft);
  assert.deepEqual(
    sendRequests
      .slice(requestStart)
      .map((input) => (input.sourceId ? input.kind : "text")),
    ["text", "image", "video", "file"],
  );
  assert.equal(await script(`return ${inline}.textContent`), "");

  // Each chat keeps its unsent files; changing targets never uploads them.
  await text("Group waiting");
  await event("paste", inline, ["not-sent.png"]);
  await staged(1);
  await click("document.querySelector('[data-thread-id=\"open\"] .chat-row')");
  await until(`return Boolean(${plain})`, Boolean);
  await staged(0);
  assert.equal(sentMedia.length, 3);
  assert.equal(await event("paste", plain, ["plain-editor.jpg"]), true);
  await staged(1);
  await remove("plain-editor.jpg");
  await staged(0);
  await until(`return document.activeElement===${plain}`, Boolean);
  await click(
    "document.querySelector('[data-thread-id=\"room@chatroom\"] .chat-row')",
  );
  await until(`return Boolean(${inline})`, Boolean);
  await staged(1);
  assert.equal(await script(`return ${inline}.textContent`), "Group waiting");
  assert.equal(
    await script(`return ${tray}.textContent.includes('not-sent.png')`),
    true,
  );
  await script(
    `const input=${inline}, range=document.createRange();range.selectNodeContents(input);range.collapse(false);window.getSelection().removeAllRanges();window.getSelection().addRange(range);input.focus();return true;`,
  );
  await event("paste", inline, [], " plain paste");
  assert.equal(
    await script(`return ${inline}.textContent`),
    "Group waiting plain paste",
  );
  assert.equal(uploadedFiles.length, 3);
  assert.equal(sentMedia.length, 3);
  await remove("not-sent.png");
  await staged(0);

  // The file input's change handler also stages, and empty-text Send is valid.
  await click("document.querySelector('[data-thread-id=\"open\"] .chat-row')");
  await until(`return Boolean(${plain})`, Boolean);
  await script(`${transfer(["selected.pdf"])}
    const input=document.querySelector('input[aria-label="Choose attachment"]');input.files=data.files;input.dispatchEvent(new Event('change',{bubbles:true}));
    const text=${plain};Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(text,'');text.dispatchEvent(new Event('input',{bubbles:true}));return true;`);
  await staged(1);
  assert.equal(
    uploadedFiles.length,
    3,
    "choosing a file cannot upload before composer Send",
  );
  assert.equal(await script(`return ${plain}.value`), "");
  await click(send);
  await staged(0);
  await mediaCount(4);
  assert.equal(uploadedFiles.at(-1).name, "selected.pdf");
  assert.equal(sentMedia.at(-1).threadId, "open");
  assert.equal(sentMedia.at(-1).kind, "file");
  assert.equal(
    sentTexts.length,
    initialTexts + 2,
    "file-only Send does not create a blank text message",
  );
  console.log(
    "Attachments: native paste/drop/file-input staging, removal/focus, rejected mention admission, double-Send guard, separate text/media messages, original quality and optional hints, per-chat drafts and file-only Send passed",
  );
}
