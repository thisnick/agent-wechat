// Clipboard/drop DOM events use synthetic files and an isolated HTTP fixture.
import assert from "node:assert/strict";

export async function attachmentsAcceptance({
  script,
  until,
  uploadedFiles,
  sentMedia,
  capture,
}) {
  const inline = "document.querySelector('.mention-editor')";
  const plain = "document.querySelector('textarea[aria-label=Message]')";
  const modal =
    "document.querySelector('[role=dialog][aria-label=\"Send attachment\"]')";
  const draft = "File draft stays here";
  const click = (target) => script(`${target}.click();return true;`);
  const event = (kind, target, files = ["photo.PNG"], text = "") =>
    script(`
    const data = new DataTransfer();
    for (const name of ${JSON.stringify(
      files
    )}) data.items.add(new File(['fixture'], name));
    if (${JSON.stringify(text)}) data.setData('text/plain', ${JSON.stringify(
      text
    )});
    const event = new Event(${JSON.stringify(
      kind
    )}, { bubbles:true, cancelable:true });
    Object.defineProperty(event, ${JSON.stringify(
      kind === "paste" ? "clipboardData" : "dataTransfer"
    )}, { value:data });
    ${target}.dispatchEvent(event);
    return event.defaultPrevented;
  `);
  const noReview = () => until(`return Boolean(${modal})`, (value) => !value);
  const cancel = async () => {
    await click(
      `${modal}.querySelector('button[aria-label="Close attachment"]')`
    );
    await noReview();
  };
  const review = () => until(`return Boolean(${modal})`, Boolean);
  assert.equal(await event("paste", inline), true);
  await noReview();
  assert.equal(
    uploadedFiles.length,
    0,
    "pasting a file while mentions are present cannot upload it"
  );
  await script(
    `${inline}.textContent=${JSON.stringify(
      draft
    )};${inline}.dispatchEvent(new Event('input',{bubbles:true}));${inline}.focus();return true;`
  );
  await until(
    "return !document.querySelector('button[aria-label=\"Attach file\"]').disabled",
    Boolean
  );

  assert.equal(
    await event("paste", inline, ["photo.PNG"], "clipboard description"),
    true
  );
  await review();
  assert.equal(
    await script(`return ${inline}.textContent`),
    draft,
    "a screenshot's text representation does not clobber the draft"
  );
  assert.equal(
    await script(`return ${modal}.querySelectorAll('select').length`),
    0
  );
  assert.equal(
    await script(`return Boolean(${modal}.querySelector('img[src^="blob:"]'))`),
    true
  );
  assert.equal(uploadedFiles.length, 0, "reviewing is entirely local");
  await capture?.("attachment-review");
  await cancel();
  assert.equal(
    await script(`return document.activeElement===${inline}`),
    true,
    "cancel restores editor focus"
  );
  assert.equal(await script(`return ${inline}.textContent`), draft);

  await event("dragenter", inline);
  await until(
    "return Boolean(document.querySelector('.composer-drop-hint'))",
    Boolean
  );
  assert.equal(
    await event("drop", inline, ["photo.PNG", "movie.MP4", "notes.pdf"]),
    true
  );
  await review();
  assert.equal(
    await script(
      `return ${modal}.querySelectorAll('.attachment-review-list li').length`
    ),
    3
  );
  assert.equal(
    await script(
      "return Boolean(document.querySelector('.composer-drop-hint'))"
    ),
    false
  );
  await click(`${modal}.querySelector('.primary')`);
  await noReview();
  for (let i = 0; i < 150 && sentMedia.length < 3; i++)
    await new Promise((resolve) => setTimeout(resolve, 20));
  assert.deepEqual(
    uploadedFiles.map((file) => file.name),
    ["photo.PNG", "movie.MP4", "notes.pdf"]
  );
  assert.deepEqual(
    sentMedia.map((item) => item.kind),
    ["image", "video", "file"]
  );
  assert.ok(sentMedia.every((item) => item.threadId === "room@chatroom"));
  assert.equal(await script(`return ${inline}.textContent`), draft);

  // Changing the conversation unmounts a pending review and cannot submit it.
  await event("paste", inline, ["not-sent.png"]);
  await review();
  await click("document.querySelector('[data-thread-id=\"open\"] .chat-row')");
  await noReview();
  await until(`return Boolean(${plain})`, Boolean);
  assert.equal(sentMedia.length, 3);
  assert.equal(await event("paste", plain, ["plain-editor.jpg"]), true);
  await review();
  await cancel();
  assert.equal(await script(`return document.activeElement===${plain}`), true);
  assert.equal(sentMedia.length, 3);
  // Ordinary text is still handled by the inline editor, with its mention rules.
  await click(
    "document.querySelector('[data-thread-id=\"room@chatroom\"] .chat-row')"
  );
  await until(`return Boolean(${inline})`, Boolean);
  await script(
    `const input=${inline}, range=document.createRange();range.selectNodeContents(input);range.collapse(false);window.getSelection().removeAllRanges();window.getSelection().addRange(range);input.focus();return true;`
  );
  await event("paste", inline, [], " plain paste");
  assert.equal(
    await script(`return ${inline}.textContent`),
    draft + " plain paste"
  );
  assert.equal(sentMedia.length, 3);
  console.log(
    "Attachments: automatic image/video/file types, clipboard/drop capture in both editors, local review/cancel/focus, mixed-file admission, intact drafts/mentions and conversation-switch cancellation passed"
  );
}
