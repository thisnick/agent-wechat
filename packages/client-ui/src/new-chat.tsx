import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { Connection } from "./connection.ts";
import { HubAvatar } from "./avatars.tsx";
import { Button } from "./ui.tsx";
import { groupEnded } from "./group-creation.ts";
import { GroupResult } from "./group-settings.tsx";
import { pause, readableError } from "./session.ts";

export function NewChat({
  connection,
  account,
  close,
  open,
}: {
  connection: Connection;
  account: string;
  close: () => void;
  open: (id: string) => void;
}) {
  const session = connection.session;
  const state = useSyncExternalStore(session.subscribe, session.snapshot);
  const creation = connection.groupCreation(account);
  const result = useSyncExternalStore(creation.subscribe, creation.snapshot);
  const [group, setGroup] = useState(!!result.receipt);
  const [query, setQuery] = useState("");
  const [name, setName] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [opening, setOpening] = useState(false),
    [error, setError] = useState("");
  const lifetime = useRef(new AbortController());
  useEffect(() => {
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    window.addEventListener("keydown", escape);
    return () => {
      window.removeEventListener("keydown", escape);
      lifetime.current.abort();
    };
  }, []);
  const own = state.accounts.find((item) => item.id === account)?.ownUsername;
  const contacts = state.contacts.filter(
    (contact) =>
      contact.id !== own &&
      contact.id !== "filehelper" &&
      !contact.id.endsWith("@chatroom"),
  );
  const visible = contacts.filter((contact) =>
    `${contact.displayName ?? ""} ${contact.username ?? contact.id}`
      .toLowerCase()
      .includes(query.toLowerCase()),
  );
  const operation = result.receipt?.operation;
  const roomId = (operation?.result as { groupId?: string })?.groupId;
  const canCreate =
    Array.isArray(state.capabilities.groupMutations) &&
    state.capabilities.groupMutations.includes("create");
  const receiptInput =
    result.receipt?.input.action === "create" ? result.receipt.input : null;
  async function openCreated() {
    if (!roomId || !roomId.endsWith("@chatroom")) return;
    setOpening(true);
    setError("");
    try {
      const refresh = await session.client.refreshGroupMembers(account, roomId);
      const deadline = Date.now() + 125000;
      while (!lifetime.current.signal.aborted && Date.now() < deadline) {
        const current = await session.client.operation(
          account,
          refresh.id,
          lifetime.current.signal,
        );
        if (
          ["succeeded", "failed", "cancelled", "expired"].includes(
            current.state,
          )
        )
          break;
        await pause(700, lifetime.current.signal);
      }
      if (
        lifetime.current.signal.aborted ||
        session.snapshot().account !== account
      )
        return;
      await session.refresh();
      if (
        lifetime.current.signal.aborted ||
        session.snapshot().account !== account
      )
        return;
      creation.clear();
      open(roomId);
    } catch (error) {
      if (!lifetime.current.signal.aborted) setError(readableError(error));
    } finally {
      if (!lifetime.current.signal.aborted) setOpening(false);
    }
  }
  return (
    <div className="overlay-backdrop" onClick={close}>
      <section
        className="modal new-chat"
        role="dialog"
        aria-modal="true"
        aria-label="New chat"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modal-head">
          <h2>New chat</h2>
          <Button icon="close" label="Close new chat" onClick={close} />
        </div>
        <div
          className="new-chat-tabs"
          role="tablist"
          aria-label="Conversation type"
        >
          <button
            role="tab"
            aria-selected={!group}
            onClick={() => setGroup(false)}
          >
            Direct message
          </button>
          <button
            role="tab"
            aria-selected={group}
            disabled={!canCreate}
            onClick={() => setGroup(true)}
          >
            New group
          </button>
        </div>
        {!canCreate && (
          <p className="field-help">
            Group creation is not available on this agent-wechat hub yet.
          </p>
        )}
        {group && result.receipt ? (
          <div className="group-result">
            <h3>{receiptInput?.name || "New group"}</h3>
            <p>{receiptInput?.members.length} contacts selected</p>
            <GroupResult model={creation} />
            {roomId ? (
              <button
                className="primary"
                disabled={opening}
                onClick={() => void openCreated()}
              >
                {opening
                  ? "Opening…"
                  : operation?.state === "succeeded"
                    ? "Open group"
                    : "Inspect group"}
              </button>
            ) : (
              <button
                className="secondary"
                disabled={result.busy}
                onClick={() => void creation.check()}
              >
                Check status
              </button>
            )}
            {operation && !groupEnded(operation) && (
              <button
                className="text-button"
                disabled={result.busy}
                onClick={() => void creation.cancel()}
              >
                Cancel if not started
              </button>
            )}
            {(result.receipt.notAdmitted ||
              operation?.state === "failed" ||
              operation?.state === "cancelled") && (
              <button className="text-button" onClick={() => creation.clear()}>
                Start again
              </button>
            )}
            {(result.error || error) && (
              <p className="attachment-error" role="alert">
                {result.error || error}
              </p>
            )}
          </div>
        ) : (
          <>
            {group && (
              <label className="group-name" htmlFor="new-group-name">
                Group name (optional)
                <input
                  id="new-group-name"
                  maxLength={80}
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                  placeholder="Name this group"
                />
              </label>
            )}
            <label className="search">
              <input
                autoFocus
                aria-label="Find contacts"
                placeholder="Find contacts"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
              />
            </label>
            <div className="new-chat-contacts">
              {visible.map((contact) =>
                group ? (
                  <label className="new-chat-contact" key={contact.id}>
                    <HubAvatar
                      connection={connection}
                      account={account}
                      username={contact.id}
                      name={contact.displayName || contact.id}
                    />
                    <span>{contact.displayName || contact.id}</span>
                    <input
                      type="checkbox"
                      aria-label={`Select ${contact.displayName || contact.id}`}
                      checked={selected.includes(contact.id)}
                      disabled={
                        !selected.includes(contact.id) && selected.length >= 40
                      }
                      onChange={() =>
                        setSelected((current) =>
                          current.includes(contact.id)
                            ? current.filter((id) => id !== contact.id)
                            : [...current, contact.id],
                        )
                      }
                    />
                  </label>
                ) : (
                  <button
                    className="new-chat-contact"
                    key={contact.id}
                    onClick={() => open(contact.id)}
                  >
                    <HubAvatar
                      connection={connection}
                      account={account}
                      username={contact.id}
                      name={contact.displayName || contact.id}
                    />
                    <span>{contact.displayName || contact.id}</span>
                  </button>
                ),
              )}
              {!visible.length && (
                <p className="no-results">No contacts found.</p>
              )}
            </div>
            {group && (
              <div className="new-chat-footer">
                <small>
                  {selected.length} selected · Choose at least 2 contacts
                </small>
                <button
                  className="primary"
                  disabled={selected.length < 2 || result.busy}
                  onClick={() =>
                    void creation.create({ name, members: selected })
                  }
                >
                  Create group
                </button>
              </div>
            )}
          </>
        )}
        {group && connection.simulator && (
          <p className="field-help">
            Creates a group in the simulator. No real WeChat invitations are
            sent.
          </p>
        )}
      </section>
    </div>
  );
}
