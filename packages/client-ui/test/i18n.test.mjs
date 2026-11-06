import test from "node:test";
import assert from "node:assert/strict";
import {
  getLocale,
  isZh,
  setLocale,
  subscribeLocale,
  t,
} from "../src/i18n.ts";
import { threadPreview } from "../src/thread-preview.ts";

test("English is the default and switching to Simplified Chinese notifies listeners", (context) => {
  context.after(() => setLocale("en"));
  assert.equal(getLocale(), "en");
  assert.equal(t("Send", "发送"), "Send");
  let calls = 0;
  const stop = subscribeLocale(() => calls++);
  setLocale("zh-CN");
  assert.equal(getLocale(), "zh-CN");
  assert.ok(isZh());
  assert.equal(t("Send", "发送"), "发送");
  setLocale("zh-CN");
  assert.equal(calls, 1, "an unchanged locale does not notify again");
  setLocale("unknown");
  assert.equal(getLocale(), "en", "unsupported values fall back to English");
  assert.equal(calls, 2);
  stop();
});

test("thread previews use the selected language for labels and default summaries", (context) => {
  context.after(() => setLocale("en"));
  const message = {
    type: 3,
    text: "Image",
    direction: "outgoing",
    senderName: null,
  };
  const thread = { id: "room@chatroom", kind: "group", lastMessage: message };
  assert.equal(threadPreview(thread), "You: Image");
  setLocale("zh-CN");
  assert.equal(threadPreview(thread), "我：图片");
  assert.equal(
    threadPreview({
      ...thread,
      lastMessage: { ...message, direction: "incoming", text: "Hello" },
    }),
    "群成员：Hello",
  );
  assert.equal(threadPreview({ ...thread, lastMessage: null }), "暂无消息");
});
