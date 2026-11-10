import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { Connection } from "./connection.ts";
import { HubAvatar } from "./avatars.tsx";
import { t } from "./i18n.ts";
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
        aria-label={t("newChat.newChat")}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modal-head">
          <h2>{t("newChat.newChat")}</h2>
          <Button
            icon="close"
            label={t("newChat.closeNewChat")}
            onClick={close}
          />
        </div>
        <div
          className="new-chat-tabs"
          role="tablist"
          aria-label={t("newChat.conversationType")}
        >
          <button
            role="tab"
            aria-selected={!group}
            onClick={() => setGroup(false)}
          >
            {t("newChat.directMessage")}
          </button>
          <button
            role="tab"
            aria-selected={group}
            disabled={!canCreate}
            onClick={() => setGroup(true)}
          >
            {t("newChat.newGroup")}
          </button>
        </div>
        {!canCreate && (
          <p className="field-help">
            {t("newChat.groupCreationNotAvailableAgent")}
          </p>
        )}
        {group && result.receipt ? (
          <div className="group-result">
            <h3>{receiptInput?.name || t("newChat.newGroup")}</h3>
            <p>
              {t("newChat.contactsSelected", { count: receiptInput?.members.length })}
            </p>
            <GroupResult model={creation} />
            {roomId ? (
              <button
                className="primary"
                disabled={opening}
                onClick={() => void openCreated()}
              >
                {opening
                  ? t("newChat.opening")
                  : operation?.state === "succeeded"
                    ? t("newChat.openGroup")
                    : t("newChat.inspectGroup")}
              </button>
            ) : (
              <button
                className="secondary"
                disabled={result.busy}
                onClick={() => void creation.check()}
              >
                {t("newChat.checkStatus")}
              </button>
            )}
            {operation && !groupEnded(operation) && (
              <button
                className="text-button"
                disabled={result.busy}
                onClick={() => void creation.cancel()}
              >
                {t("newChat.cancelIfNotStarted")}
              </button>
            )}
            {(result.receipt.notAdmitted ||
              operation?.state === "failed" ||
              operation?.state === "cancelled") && (
              <button className="text-button" onClick={() => creation.clear()}>
                {t("newChat.startAgain")}
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
                {t("newChat.groupNameOptional")}
                <input
                  id="new-group-name"
                  maxLength={80}
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                  placeholder={t("newChat.nameGroup")}
                />
              </label>
            )}
            <label className="search">
              <input
                autoFocus
                aria-label={t("newChat.findContacts")}
                placeholder={t("newChat.findContacts")}
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
                      aria-label={t("newChat.select", { value: contact.displayName || contact.id })}
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
                <p className="no-results">
                  {t("newChat.noContactsFound")}
                </p>
              )}
            </div>
            {group && (
              <div className="new-chat-footer">
                <small>
                  {t("newChat.selectedChooseLeast2Contacts", { count: selected.length })}
                </small>
                <button
                  className="primary"
                  disabled={selected.length < 2 || result.busy}
                  onClick={() =>
                    void creation.create({ name, members: selected })
                  }
                >
                  {t("newChat.createGroup")}
                </button>
              </div>
            )}
          </>
        )}
        {group && connection.simulator && (
          <p className="field-help">
            {t("newChat.createsGroupSimulatorNoReal")}
          </p>
        )}
      </section>
    </div>
  );
}
