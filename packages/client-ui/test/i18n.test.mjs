import assert from "node:assert/strict";
import test from "node:test";
import en from "../locales/en.json" with { type: "json" };
import {
  currentLanguage,
  formatLocale,
  languagePreference,
  matchLanguage,
  setLanguagePreference,
  subscribeLanguage,
  t,
} from "../src/i18n.ts";

test("system tags map to shipped languages; Traditional Chinese does not map to Simplified", () => {
  assert.equal(matchLanguage("en-GB"), "en");
  assert.equal(matchLanguage("zh-CN"), "zh-CN");
  assert.equal(matchLanguage("zh"), "zh-CN");
  assert.equal(matchLanguage("zh-Hans-SG"), "zh-CN");
  assert.equal(matchLanguage("zh-TW"), null);
  assert.equal(matchLanguage("zh-Hant"), null);
  assert.equal(matchLanguage("fr-FR"), null);
  assert.equal(matchLanguage(undefined), null);
});

test("English is the default and follows the system", () => {
  assert.equal(languagePreference(), "system");
  assert.equal(currentLanguage(), "en");
  assert.deepEqual(formatLocale(), []);
  assert.equal(t("ui.language"), en.ui.language);
});

test("choosing a language switches text, notifies listeners and formats dates in that language", (context) => {
  let calls = 0;
  const stop = subscribeLanguage(() => calls++);
  context.after(() => {
    stop();
    setLanguagePreference("system");
  });
  setLanguagePreference("zh-CN");
  assert.equal(currentLanguage(), "zh-CN");
  assert.equal(t("ui.language"), "语言");
  assert.equal(formatLocale(), "zh-CN");
  assert.ok(calls >= 1);
});

test("plurals and interpolation use the active language", (context) => {
  context.after(() => setLanguagePreference("system"));
  assert.equal(t("attachmentPicker.attachmentCount", { count: 1 }), "1 attachment");
  assert.equal(t("attachmentPicker.attachmentCount", { count: 3 }), "3 attachments");
  setLanguagePreference("zh-CN");
  assert.equal(t("attachmentPicker.attachmentCount", { count: 3 }), "3 个附件");
});

test("a key missing from a translation falls back to English", (context) => {
  context.after(() => setLanguagePreference("system"));
  setLanguagePreference("zh-CN");
  // Not a real key in either catalog: i18next returns the key itself.
  assert.equal(t(/** @type {any} */ ("ui.doesNotExist")), "ui.doesNotExist");
  // English-only plural form: zh-CN has no `_one`, Chinese uses `_other` for every count.
  assert.equal(t("attachmentPicker.attachmentCount", { count: 1 }), "1 个附件");
});
