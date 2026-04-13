import type { Message } from "@agent-wechat/shared";

export function formatPaymentBody(msg: Message): string | undefined {
  if (msg.kind !== "transfer" && msg.kind !== "red_packet") {
    return undefined;
  }

  const label = msg.kind === "transfer" ? "Transfer" : "Red packet";
  const status = msg.isReceived == null
    ? undefined
    : msg.isReceived
      ? "received"
      : "unreceived";
  const parts = [label];

  if (msg.payment?.amountText) {
    parts.push(msg.payment.amountText);
  }
  if (status) {
    parts.push(status);
  }

  const metadata = [`localId: ${msg.localId}`];
  if (msg.kind === "transfer" && msg.payment?.transactionId) {
    metadata.push(`transactionId: ${msg.payment.transactionId}`);
  }
  if (msg.kind === "red_packet" && msg.payment?.sendId) {
    metadata.push(`sendId: ${msg.payment.sendId}`);
  }
  if (msg.kind === "red_packet" && msg.payment?.payMsgId) {
    metadata.push(`payMsgId: ${msg.payment.payMsgId}`);
  }

  const summary = `[${parts.join(" - ")}] ${msg.content || ""}`.trim();
  return `${summary} (${metadata.join(", ")})`;
}
