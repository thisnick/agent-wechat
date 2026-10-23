import { createContext, useContext } from "react";
import type { NativeMessage } from "@agent-wechat/shared";

export const ReplyContext = createContext<{
  enabled: boolean;
  reply: (message: NativeMessage) => void;
} | null>(null);
export const useReply = () => useContext(ReplyContext);
