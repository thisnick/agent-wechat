import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { Connection } from "./connection.ts";
import { HubAvatar } from "./avatars.tsx";
import { t, useLocale } from "./i18n.ts";
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
  useLocale();
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
        aria-label={t("New chat", "发起聊天")}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modal-head">
          <h2>{t("New chat", "发起聊天")}</h2>
          <Button
            icon="close"
            label={t("Close new chat", "关闭发起聊天")}
            onClick={close}
          />
        </div>
        <div
          className="new-chat-tabs"
          role="tablist"
          aria-label={t("Conversation type", "会话类型")}
        >
          <button
            role="tab"
            aria-selected={!group}
            onClick={() => setGroup(false)}
          >
            {t("Direct message", "私聊")}
          </button>
          <button
            role="tab"
            aria-selected={group}
            disabled={!canCreate}
            onClick={() => setGroup(true)}
          >
            {t("New group", "新建群聊")}
          </button>
        </div>
        {!canCreate && (
          <p className="field-help">
            {t(
              "Group creation is not available on this agent-wechat hub yet.",
              "此 agent-wechat Hub 暂不支持创建群聊。",
            )}
          </p>
        )}
        {group && result.receipt ? (
          <div className="group-result">
            <h3>{receiptInput?.name || t("New group", "新建群聊")}</h3>
            <p>
              {t(
                `${receiptInput?.members.length} contacts selected`,
                `已选择 ${receiptInput?.members.length} 位联系人`,
              )}
            </p>
            <GroupResult model={creation} />
            {roomId ? (
              <button
                className="primary"
                disabled={opening}
                onClick={() => void openCreated()}
              >
                {opening
                  ? t("Opening…", "正在打开…")
                  : operation?.state === "succeeded"
                    ? t("Open group", "打开群聊")
                    : t("Inspect group", "查看群聊")}
              </button>
            ) : (
              <button
                className="secondary"
                disabled={result.busy}
                onClick={() => void creation.check()}
              >
                {t("Check status", "检查状态")}
              </button>
            )}
            {operation && !groupEnded(operation) && (
              <button
                className="text-button"
                disabled={result.busy}
                onClick={() => void creation.cancel()}
              >
                {t("Cancel if not started", "若未开始则取消")}
              </button>
            )}
            {(result.receipt.notAdmitted ||
              operation?.state === "failed" ||
              operation?.state === "cancelled") && (
              <button className="text-button" onClick={() => creation.clear()}>
                {t("Start again", "重新开始")}
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
                {t("Group name (optional)", "群名称（可选）")}
                <input
                  id="new-group-name"
                  maxLength={80}
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                  placeholder={t("Name this group", "输入群名称")}
                />
              </label>
            )}
            <label className="search">
              <input
                autoFocus
                aria-label={t("Find contacts", "查找联系人")}
                placeholder={t("Find contacts", "查找联系人")}
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
                      aria-label={t(
                        `Select ${contact.displayName || contact.id}`,
                        `选择 ${contact.displayName || contact.id}`,
                      )}
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
                  {t("No contacts found.", "未找到联系人。")}
                </p>
              )}
            </div>
            {group && (
              <div className="new-chat-footer">
                <small>
                  {t(
                    `${selected.length} selected · Choose at least 2 contacts`,
                    `已选 ${selected.length} 人 · 请至少选择 2 位联系人`,
                  )}
                </small>
                <button
                  className="primary"
                  disabled={selected.length < 2 || result.busy}
                  onClick={() =>
                    void creation.create({ name, members: selected })
                  }
                >
                  {t("Create group", "创建群聊")}
                </button>
              </div>
            )}
          </>
        )}
        {group && connection.simulator && (
          <p className="field-help">
            {t(
              "Creates a group in the simulator. No real WeChat invitations are sent.",
              "在模拟器中创建群聊，不会发送真实的微信群邀请。",
            )}
          </p>
        )}
      </section>
    </div>
  );
}
