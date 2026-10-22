import { useEffect, useSyncExternalStore } from "react";
import type { Connection } from "./connection.ts";
import { Avatar } from "./ui.tsx";
export function HubAvatar({
  connection,
  account,
  username,
  name,
}: {
  connection: Connection;
  account: string;
  username?: string | null;
  name: string;
}) {
  const cache = connection.avatars;
  const url = useSyncExternalStore(cache.subscribe, () =>
    cache.get(account, username ?? ""),
  );
  useEffect(() => {
    if (username) void cache.load(account, username);
  }, [cache, account, username]);
  return <Avatar name={name} src={url} />;
}
