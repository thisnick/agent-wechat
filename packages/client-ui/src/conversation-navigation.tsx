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
    null,
  );
  const pending = useRef<AbortController | null>(null);
  useEffect(() => () => pending.current?.abort(), []);
  async function jump(message: NativeMessage) {
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
      if (
        !(await session.revealMessage(message.id, controller.signal)) ||
        controller.signal.aborted
      )
        return false;
      setTarget({ id: message.id, sequence: Date.now() });
      return true;
    } catch {
      return false;
    }
  }
  return (
    <Navigation.Provider value={{ jump, target }}>
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
      }),
    );
    return () => cancelAnimationFrame(id);
  }, [navigation?.target, scrollToMessage]);
  return null;
}
