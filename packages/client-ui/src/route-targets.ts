import { NativeApiError, type NativeWeChatClient } from "@agent-wechat/shared";

type Lookup = Pick<NativeWeChatClient, "thread" | "contact" | "group" | "message">;

const missing = (error: unknown) => error instanceof NativeApiError && error.status === 404;

/**
 * Whether a conversation named by a link can be opened. A conversation exists
 * if the account lists it or the hub knows it as a thread, contact or group. A
 * failed lookup other than "not found" does not disprove the link, so it counts
 * as available and opening it reports the real error.
 */
export async function conversationAvailable(
  client: Lookup,
  account: string,
  thread: string,
  listed: readonly { id: string }[],
): Promise<boolean> {
  if (thread === "filehelper" || listed.some((item) => item.id === thread)) return true;
  const lookups: (() => Promise<unknown>)[] = [
    () => client.thread(account, thread),
    () => client.contact(account, thread),
    ...(thread.endsWith("@chatroom") ? [() => client.group(account, thread)] : []),
  ];
  for (const lookup of lookups) {
    try {
      await lookup();
      return true;
    } catch (error) {
      if (!missing(error)) return true;
    }
  }
  return false;
}

/** Whether a message named by a link belongs to the conversation named with it. */
export async function messageAvailable(
  client: Lookup,
  account: string,
  thread: string,
  message: string,
): Promise<boolean> {
  try {
    const found = await client.message(account, message);
    return found.accountId === account && found.threadId === thread;
  } catch (error) {
    return !missing(error);
  }
}
