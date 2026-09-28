type SchedulerOptions = {
  signal: AbortSignal;
  directLimit?: number;
  groupLimit?: number;
  onError?: (error: unknown, chatId: string) => void;
};

/** Revisit busy chats on the next poll instead of queuing stale cursor snapshots. */
export function createChatScheduler({ signal, directLimit = 2, groupLimit = 2, onError = () => {} }: SchedulerOptions) {
  const active = new Map<string, Promise<void>>();
  const counts = { direct: 0, group: 0 };
  const limits = { direct: directLimit, group: groupLimit };
  return {
    has: (chatId: string) => active.has(chatId),
    schedule(chatId: string, task: () => void | Promise<void>): boolean {
      const lane = chatId.includes("@chatroom") ? "group" : "direct";
      if (signal.aborted || active.has(chatId) || counts[lane] >= limits[lane]) return false;
      counts[lane]++;
      const work = Promise.resolve().then(() => {
        signal.throwIfAborted();
        return task();
      }).catch((error: unknown) => {
        if (!signal.aborted) onError(error, chatId);
      }).finally(() => {
        active.delete(chatId);
        counts[lane]--;
      });
      active.set(chatId, work);
      return true;
    },
    async drain(): Promise<void> {
      await Promise.allSettled([...active.values()]);
    },
  };
}

/** Serialize snapshots and renames so an older save cannot overwrite a newer one. */
export function createSerialQueue() {
  let tail: Promise<unknown> = Promise.resolve();
  return {
    run<T>(task: () => T | Promise<T>): Promise<T> {
      const result = tail.then(task);
      tail = result.catch(() => {});
      return result;
    },
    drain: () => tail,
  };
}
