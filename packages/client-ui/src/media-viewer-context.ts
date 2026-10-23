import { createContext, useContext } from "react";
import type { NativeMessage } from "@agent-wechat/shared";

export const MediaViewerContext = createContext<{
  open: (message: NativeMessage) => void;
} | null>(null);
export const useMediaViewer = () => useContext(MediaViewerContext);
