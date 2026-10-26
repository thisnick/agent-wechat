import assert from "node:assert/strict";
import { spawn } from "node:child_process";

// A separate browser has no Tauri API. Exercise the ordinary web connection form.
export async function browserNavigationAcceptance(origin) {
  const driver = spawn("WebKitWebDriver", ["--port=4450"], { stdio: "ignore" });
  let session;
  const pause = () => new Promise((resolve) => setTimeout(resolve, 100));
  async function call(
    path,
    body,
    method = body === undefined ? "GET" : "POST"
  ) {
    const response = await fetch("http://127.0.0.1:4450" + path, {
      method,
      headers: { "Content-Type": "application/json" },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      signal: AbortSignal.timeout(20000),
    });
    const data = await response.json();
    if (!response.ok || data.value?.error)
      throw new Error(JSON.stringify(data));
    return data.value;
  }
  const script = (source) =>
    call(`/session/${session}/execute/sync`, { script: source, args: [] });
  async function wait(source, check) {
    for (let n = 0; n < 100; n++) {
      const value = await script(source);
      if (check(value)) return value;
      await pause();
    }
    throw new Error(
      "Browser navigation timed out: " +
        (await script("return document.body.innerText"))
    );
  }
  try {
    for (let n = 0; n < 100; n++) {
      try {
        await call("/status");
        break;
      } catch (error) {
        if (n === 99) throw error;
        await pause();
      }
    }
    session = (
      await call("/session", {
        capabilities: { alwaysMatch: { browserName: "MiniBrowser" } },
      })
    ).sessionId;
    await call(`/session/${session}/url`, { url: origin + "/browser" });
    await wait("return Boolean(document.querySelector('#hub-key'))", Boolean);
    await script(
      "const input=document.querySelector('#hub-key'); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,'a'.repeat(64)); input.dispatchEvent(new Event('input',{bubbles:true})); return true"
    );
    await script(
      "document.querySelector('button.continue-button').click(); return true"
    );
    await wait(
      "return Boolean(document.querySelector('.login-account'))",
      Boolean
    );
    await script(
      "[...document.querySelectorAll('button')].find(b=>b.textContent.includes('Connect to a different')).click(); return true"
    );
    await wait("return Boolean(document.querySelector('.step-back'))", Boolean);
    await script(
      "const input=document.querySelector('#hub-address'); Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,'http://127.0.0.1:1'); input.dispatchEvent(new Event('input',{bubbles:true})); document.querySelector('.step-back').click(); return true"
    );
    await wait(
      "return Boolean(document.querySelector('.login-account'))",
      Boolean
    );
    assert.equal(
      await script(
        "return document.querySelector('.login-account').textContent.includes('Interface fixture')"
      ),
      true
    );
    console.log(
      "Browser Back returns to the previous hub without connecting to the edited address"
    );
  } finally {
    if (session)
      await call(`/session/${session}`, undefined, "DELETE").catch(() => {});
    driver.kill("SIGTERM");
  }
}
