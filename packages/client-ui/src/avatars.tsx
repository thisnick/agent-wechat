import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { Connection } from "./connection.ts";
import type { NativeThreadAvatarMember } from "@agent-wechat/shared";
import { Avatar, Icon } from "./ui.tsx";

export function ConversationAvatar({
  connection,
  account,
  username,
  name,
  members,
}: {
  connection: Connection;
  account: string;
  username: string;
  name: string;
  members?: NativeThreadAvatarMember[];
}) {
  if (!username.endsWith("@chatroom"))
    return (
      <span className="conversation-avatar" aria-hidden="true">
        <HubAvatar
          connection={connection}
          account={account}
          username={username}
          name={name}
        />
      </span>
    );
  const faces = (members ?? [])
    .filter(
      (member, index, all) =>
        all.findIndex((other) => other.id === member.id) === index,
    )
    .slice(0, 4);
  if (faces.length === 0)
    return (
      <RoomAvatar connection={connection} account={account} username={username} />
    );

  return (
    <span
      className="conversation-avatar group-avatar has-members"
      aria-hidden="true"
    >
      {faces.map((member) => (
        <HubAvatar
          key={member.id}
          connection={connection}
          account={account}
          username={member.id}
          name={member.displayName}
        />
      ))}
      {faces.length === 1 && (
        <span className="group-avatar-placeholder">
          <Icon name="people" />
        </span>
      )}
    </span>
  );
}

function RoomAvatar({
  connection,
  account,
  username,
}: {
  connection: Connection;
  account: string;
  username: string;
}) {
  const { element, url } = useHubAvatar(connection, account, username);
  return (
    <span
      ref={element}
      className="conversation-avatar group-avatar"
      aria-hidden="true"
    >
      {url ? <img src={url} alt="" /> : <Icon name="people" />}
    </span>
  );
}

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
  const { element, url } = useHubAvatar(connection, account, username);
  return <Avatar ref={element} name={name} src={url} />;
}

function useHubAvatar(
  connection: Connection,
  account: string,
  username?: string | null,
) {
  const cache = connection.avatars;
  const opening = useSyncExternalStore(
    connection.session.subscribe,
    connection.session.snapshot,
  ).opening;
  const [visible, setVisible] = useState(false);
  const element = useRef<HTMLSpanElement>(null);
  const url = useSyncExternalStore(cache.subscribe, () =>
    cache.get(account, username ?? ""),
  );
  useEffect(() => {
    if (!username) return;
    const target = element.current;
    if (!target) return;
    const observer = new IntersectionObserver((entries) => {
      setVisible(
        entries.some(
          (entry) => entry.isIntersecting && entry.intersectionRatio > 0,
        ),
      );
    });
    observer.observe(target);
    return () => observer.disconnect();
  }, [cache, account, username]);
  useEffect(() => {
    if (!username || !visible) return;
    return cache.show(account, username);
  }, [cache, account, username, visible, opening]);
  return { element, url };
}
