import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useMessageScroller } from "@shadcn/react/message-scroller";
import type { NativeMessage } from "@agent-wechat/shared";
import type { HubSession } from "./session.ts";

const Navigation = createContext<{
  jump: (message: NativeMessage) => Promise<boolean>;
  jumpQuote: (message: NativeMessage) => Promise<boolean>;
  /** Scrolls to a message of the open conversation by its ID, loading older history if needed. */
  reveal: (id: string) => Promise<boolean>;
  target: { id: string; sequence: number } | null;
} | null>(null);
export const useConversationNavigation = () => useContext(Navigation);

export function ConversationNavigation({
  session,
  closeSearch,
  children,
}: {
  session: HubSession;
  closeSearch: () => void;
  children: ReactNode;
}) {
  const [target, setTarget] = useState<{ id: string; sequence: number } | null>(
    null
  );
  const pending = useRef<AbortController | null>(null);
  useEffect(() => () => pending.current?.abort(), []);
  async function navigate(
    message: Pick<NativeMessage, "accountId" | "threadId">,
    resolve: (signal: AbortSignal) => Promise<string | undefined>
  ) {
    pending.current?.abort();
    const controller = new AbortController();
    pending.current = controller;
    const state = session.snapshot();
    if (
      state.account !== message.accountId ||
      state.selected !== message.threadId
    )
      return false;
    closeSearch();
    try {
      const id = await resolve(controller.signal);
      if (!id || controller.signal.aborted) return false;
      setTarget({ id, sequence: Date.now() });
      return true;
    } catch {
      return false;
    }
  }
  const jump = (message: NativeMessage) =>
    navigate(message, async (signal) =>
      (await session.revealMessage(message.id, signal)) ? message.id : undefined
    );
  const jumpQuote = (message: NativeMessage) =>
    navigate(
      message,
      async (signal) => (await session.revealQuote(message, signal))?.id
    );
  const reveal = (id: string) => {
    const { account, selected } = session.snapshot();
    return navigate(
      { accountId: account ?? "", threadId: selected ?? "" },
      async (signal) => ((await session.revealMessage(id, signal)) ? id : undefined)
    );
  };
  return (
    <Navigation.Provider value={{ jump, jumpQuote, reveal, target }}>
      {children}
    </Navigation.Provider>
  );
}

export function ScrollToQuotedMessage() {
  const navigation = useConversationNavigation();
  const { scrollToMessage } = useMessageScroller();
  useEffect(() => {
    if (!navigation?.target) return;
    const id = requestAnimationFrame(() =>
      scrollToMessage(navigation.target!.id, {
        align: "center",
        behavior: "smooth",
      })
    );
    return () => cancelAnimationFrame(id);
  }, [navigation?.target, scrollToMessage]);
  return null;
}
