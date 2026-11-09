import { useEffect, useRef } from "react";
import type { HubSession } from "./session.ts";
import { messageAvailable } from "./route-targets.ts";
import { useConversationNavigation } from "./conversation-navigation.tsx";

/**
 * Scrolls the conversation to the message named by the address and highlights
 * it, once that conversation is open. `missing` runs when the message is not
 * part of this conversation.
 */
export function RouteMessage({
  session,
  account,
  thread,
  message,
  missing,
}: {
  session: HubSession;
  account: string;
  thread: string;
  message: string | null;
  missing: () => void;
}) {
  const navigation = useConversationNavigation();
  const report = useRef(missing);
  report.current = missing;
  useEffect(() => {
    if (!message || !navigation) return;
    let cancelled = false;
    void (async () => {
      const found =
        (await messageAvailable(session.client, account, thread, message)) &&
        (await navigation.reveal(message));
      if (!found && !cancelled) report.current();
    })();
    return () => {
      cancelled = true;
    };
  }, [session, account, thread, message]);
  return null;
}
