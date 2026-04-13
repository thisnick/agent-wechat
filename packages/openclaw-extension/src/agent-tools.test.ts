import test from "node:test";
import assert from "node:assert/strict";
import type { ResolvedWeChatAccount } from "./types.ts";
import {
  createWeChatReceiveTransferTool,
} from "./agent-tools.ts";
import { formatPaymentBody } from "./payment-format.ts";

function baseAccount(overrides: Partial<ResolvedWeChatAccount> = {}): ResolvedWeChatAccount {
  return {
    accountId: "default",
    enabled: true,
    serverUrl: "http://localhost:6174",
    token: "token",
    dmPolicy: "open",
    allowFrom: [],
    groupPolicy: "open",
    groupAllowFrom: [],
    groups: {},
    pollIntervalMs: 1000,
    authPollIntervalMs: 30000,
    ...overrides,
  };
}

test("wechat_receive_transfer posts the targeted transfer request", async () => {
  const tool = createWeChatReceiveTransferTool(baseAccount());
  const calls: Array<{ url: string; init?: RequestInit }> = [];
  const originalFetch = globalThis.fetch;

  globalThis.fetch = async (input: URL | RequestInfo, init?: RequestInit) => {
    calls.push({ url: String(input), init });
    return new Response(
      JSON.stringify({
        success: true,
        kind: "transfer",
        localId: 16,
        amountText: "￥0.30",
        transactionId: "tx-16",
        transferId: "tr-16",
        receivedAt: "2026-04-13T10:00:00Z",
      }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );
  };

  try {
    const result = await tool.execute("tool-call", {
      chatId: "wechat:APEX_GLORY",
      localId: 16,
    });

    assert.equal(calls.length, 1);
    assert.equal(
      calls[0].url,
      "http://localhost:6174/api/messages/APEX_GLORY/transfer/receive",
    );
    assert.equal(calls[0].init?.method, "POST");
    assert.equal(
      calls[0].init?.body,
      JSON.stringify({ transactionId: undefined, localId: 16 }),
    );
    assert.match(result.content[0]?.text ?? "", /Received the WeChat transfer ￥0.30/);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("formatPaymentBody includes status and targeting metadata", () => {
  const text = formatPaymentBody({
    localId: 16,
    serverId: 16,
    chatId: "APEX_GLORY",
    sender: "APEX_GLORY",
    senderName: "APEX_GLORY",
    type: 49,
    kind: "transfer",
    appMsgType: 2000,
    content: "微信转账 ￥0.30",
    timestamp: "2026-04-13T10:00:00Z",
    isSelf: false,
    isReceived: false,
    payment: {
      kind: "transfer",
      appMsgType: 2000,
      amountText: "￥0.30",
      amountCents: 30,
      currency: "CNY",
      transactionId: "tx-16",
      transferId: "tr-16",
    },
  });

  assert.equal(
    text,
    "[Transfer - ￥0.30 - unreceived] 微信转账 ￥0.30 (localId: 16, transactionId: tx-16)",
  );
});
