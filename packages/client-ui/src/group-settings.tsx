import {
  useCallback,
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import {
  isNativeTombstone,
  type NativeGroup,
  type NativeGroupMember,
  type NativeOperation,
} from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import {
  GroupMutations,
  groupEnded,
  type GroupAction,
} from "./group-creation.ts";
import { HubAvatar } from "./avatars.tsx";
import { t, useLocale } from "./i18n.ts";
import { Button } from "./ui.tsx";
import { inventory, pause, readableError } from "./session.ts";

function groupLabel(action: GroupAction["action"]): string {
  switch (action) {
    case "create":
      return t("Create group", "创建群聊");
    case "rename":
      return t("Rename group", "修改群名称");
    case "leave":
      return t("Leave group", "退出群聊");
    case "add_members":
      return t("Add members", "添加成员");
    case "remove_members":
      return t("Remove members", "移除成员");
    case "my_name":
      return t("My name in this group", "我在本群的昵称");
    case "clear_announcement":
      return t("Clear announcement", "清空群公告");
    case "mute":
      return t("Notifications", "通知");
    case "pin":
      return t("Pin chat", "置顶聊天");
  }
}
type Result = {
  groupId?: string;
  remoteOutcome?: string;
  memberOutcome?: string;
  profileProjection?: string;
  members?: { username: string; status: number | null }[];
  unconfirmedMembers?: string[];
  steps?: { accepted: boolean }[];
};
export function GroupResult({ model }: { model: GroupMutations }) {
  useLocale();
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  const receipt = state.receipt;
  if (!receipt || ["pin", "mute"].includes(receipt.input.action)) return null;
  const operation = receipt.operation;
  const result = operation?.result as Result | undefined;
  const partial = result?.memberOutcome === "partial";
  return (
    <div className="group-operation-result">
      <p role="status">
        {receipt.notAdmitted
          ? t("The change wasn’t submitted.", "更改未提交。")
          : operation?.state === "succeeded"
          ? partial
            ? receipt.input.action === "create"
              ? t(
                  "Group created. Some selected contacts weren’t confirmed as added.",
                  "群聊已创建。部分所选联系人未确认已加入。",
                )
              : t(
                  "Group updated. Some member changes weren’t confirmed.",
                  "群聊已更新。部分成员变更未确认。",
                )
            : receipt.input.action === "leave"
            ? t(
                "You left the group. Cached history is still available.",
                "你已退出群聊，已缓存的聊天记录仍可查看。",
              )
            : receipt.input.action === "create"
            ? t("Group created.", "群聊已创建。")
            : t("Group updated.", "群聊已更新。")
          : operation?.state === "needs_review"
          ? t(
              "The outcome couldn’t be confirmed. Check the group before attempting this change again.",
              "无法确认操作结果。请先检查群聊状态，再尝试重新操作。",
            )
          : operation?.state === "failed"
          ? t(
              "The change failed. Review the result before trying again.",
              "更改失败，请检查结果后再重试。",
            )
          : operation?.state === "cancelled"
          ? t("Cancelled before submission.", "提交前已取消。")
          : operation
          ? t("Updating group…", "正在更新群聊…")
          : t(
              "Checking whether the change was submitted…",
              "正在检查更改是否已提交…",
            )}
      </p>
      {result?.members && result.members.length > 0 && (
        <ul className="group-member-results">
          {result.members.map((member) => (
            <li key={member.username}>
              <span>{member.username}</span>
              <small>
                {receipt.input.action === "remove_members" &&
                member.status === null
                  ? t("Removal acknowledged", "移除已确认")
                  : member.status === 0
                  ? t("Accepted", "已接受")
                  : t(
                      `Not confirmed (status ${member.status ?? "unknown"})`,
                      `未确认（状态 ${member.status ?? "未知"}）`,
                    )}
              </small>
            </li>
          ))}
        </ul>
      )}
      {!!result?.unconfirmedMembers?.length && (
        <p className="field-help">
          {t(
            `Unconfirmed: ${result.unconfirmedMembers.join(", ")}. These members were not assumed to have changed.`,
            `未确认：${result.unconfirmedMembers.join(", ")}。这些成员状态未视为已变更。`,
          )}
        </p>
      )}
      {result?.steps?.some((step) => !step.accepted) && (
        <p className="field-help">
          {t("One or more steps were rejected. ", "一个或多个步骤被拒绝。")}
          {receipt.input.action === "leave"
            ? t(
                "The final quit result is shown above; cleanup results are retained separately.",
                "最终退群结果已显示在上方；清理结果单独保留。",
              )
            : t(
                "Refresh the group to inspect its current state.",
                "请刷新群聊以查看当前状态。",
              )}
        </p>
      )}
      {result?.remoteOutcome === "accepted" &&
        result.profileProjection === "pending" && (
          <p className="field-help">
            {t(
              "WeChat accepted this change. The saved view on this agent-wechat hub is still updating.",
              "微信已接受此更改，此 agent-wechat Hub 上的保存视图仍在更新中。",
            )}
          </p>
        )}
      {partial && (
        <p className="field-help">
          {t(
            "No members will be retried automatically.",
            "不会自动重试任何成员。",
          )}
        </p>
      )}
      {state.error && (
        <p className="attachment-error" role="alert">
          {state.error}
        </p>
      )}
    </div>
  );
}

export function GroupSettings({
  connection,
  account,
  groupId,
}: {
  connection: Connection;
  account: string;
  groupId: string;
}) {
  useLocale();
  const session = connection.session;
  const state = useSyncExternalStore(session.subscribe, session.snapshot);
  const model = connection.groupCreation(account, groupId);
  const operation = useSyncExternalStore(model.subscribe, model.snapshot);
  const [group, setGroup] = useState<NativeGroup | null>(null);
  const [members, setMembers] = useState<NativeGroupMember[]>([]);
  const [error, setError] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [mode, setMode] = useState<GroupAction["action"] | "result" | null>(
    null,
  );
  const [draft, setDraft] = useState("");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const lifetime = useRef<AbortController | null>(null);
  const loadNumber = useRef(0);
  const own = state.accounts.find((item) => item.id === account)?.ownUsername;
  const capabilities = Array.isArray(state.capabilities.groupMutations)
    ? state.capabilities.groupMutations
    : [];
  const enabled = (action: GroupAction["action"]) =>
    state.connected &&
    !(
      ["pin", "mute"].includes(operation.receipt?.input.action ?? "") &&
      (!groupEnded(operation.receipt?.operation) ||
        operation.receipt?.operation?.state === "needs_review")
    ) &&
    group?.isInGroup === true &&
    (action === "mute"
      ? Array.isArray(state.capabilities.threadNotificationTargets) &&
        state.capabilities.threadNotificationTargets.includes("group")
      : capabilities.includes(action));
  const load = useCallback(async () => {
    const number = ++loadNumber.current;
    const signal = lifetime.current?.signal;
    const [info, roster] = await Promise.all([
      session.client.group(account, groupId, signal),
      inventory((cursor) =>
        session.client.groupMembers(
          account,
          groupId,
          { limit: 200, cursor },
          signal,
        ),
      ),
    ]);
    if (
      !signal ||
      signal.aborted ||
      number !== loadNumber.current ||
      session.snapshot().account !== account
    )
      return;
    setGroup(info);
    setMembers(
      roster.items.filter(
        (member) => !isNativeTombstone(member),
      ) as NativeGroupMember[],
    );
  }, [account, groupId, session]);
  useEffect(() => {
    const controller = new AbortController();
    lifetime.current = controller;
    return () => {
      controller.abort();
      loadNumber.current++;
    };
  }, []);
  useEffect(() => {
    void load().catch((e) => {
      if (!lifetime.current?.signal.aborted) setError(readableError(e));
    });
  }, [load, state.selectedGroup, state.contacts]);
  const receiptState = operation.receipt?.operation?.state;
  useEffect(() => {
    if (
      !["pin", "mute"].includes(operation.receipt?.input.action ?? "") &&
      receiptState &&
      groupEnded(operation.receipt?.operation)
    ) {
      void load().catch((e) => {
        if (!lifetime.current?.signal.aborted) setError(readableError(e));
      });
      void session.refresh().catch((e) => {
        if (!lifetime.current?.signal.aborted) setError(readableError(e));
      });
    }
  }, [receiptState]);
  async function waitMetadata(value: NativeOperation) {
    const signal = lifetime.current!.signal;
    const end = Date.now() + 125000;
    while (!signal.aborted && Date.now() < end) {
      const item = await session.client.operation(account, value.id, signal);
      if (item.state === "succeeded") return;
      if (
        ["failed", "cancelled", "needs_review", "expired"].includes(item.state)
      )
        throw new Error(
          t(
            "The group refresh did not finish. The saved view may be incomplete.",
            "群聊刷新未完成，保存的视图可能不完整。",
          ),
        );
      await pause(700, signal);
    }
    throw new Error(
      t(
        "Group refresh is still pending. Try Refresh group again to inspect the latest view.",
        "群聊刷新仍在进行中，请稍后再次点击“刷新群聊”查看最新视图。",
      ),
    );
  }
  async function refresh() {
    if (refreshing) return;
    setRefreshing(true);
    setError("");
    try {
      await waitMetadata(await session.client.refreshGroup(account, groupId));
      if (lifetime.current!.signal.aborted) return;
      await waitMetadata(
        await session.client.refreshGroupMembers(account, groupId),
      );
      await load();
      await session.refresh();
    } catch (e) {
      if (!lifetime.current!.signal.aborted) setError(readableError(e));
    } finally {
      if (!lifetime.current!.signal.aborted) setRefreshing(false);
    }
  }
  function edit(action: GroupAction["action"]) {
    if (
      operation.receipt &&
      ["pin", "mute"].includes(operation.receipt.input.action)
    ) {
      if (
        !groupEnded(operation.receipt.operation) ||
        operation.receipt.operation?.state === "needs_review"
      )
        return;
      model.clear();
    } else if (operation.receipt) {
      setMode("result");
      return;
    }
    setMode(action);
    setQuery("");
    setSelected([]);
    setDraft(
      action === "rename"
        ? group?.name || group?.displayName || ""
        : action === "my_name"
        ? group?.myDisplayName || ""
        : "",
    );
  }
  async function submit() {
    if (!mode || mode === "result" || mode === "create" || mode === "pin")
      return;
    let input: GroupAction;
    if (mode === "rename") input = { action: mode, groupId, name: draft };
    else if (mode === "my_name")
      input = { action: mode, groupId, displayName: draft };
    else if (mode === "add_members" || mode === "remove_members")
      input = { action: mode, groupId, members: selected };
    else if (mode === "mute")
      input = { action: mode, groupId, muted: group?.muted !== true };
    else input = { action: mode, groupId };
    await model.run(input);
  }
  const choices =
    mode === "remove_members"
      ? members.map((member) => ({
          id: member.id,
          displayName:
            member.roomDisplayName || member.displayName || member.id,
        }))
      : state.contacts.filter(
          (contact) =>
            contact.id !== "filehelper" &&
            !contact.id.endsWith("@chatroom") &&
            !members.some((member) => member.id === contact.id),
        );
  const visible = choices.filter(
    (item) =>
      item.id !== own &&
      `${item.displayName} ${item.id}`
        .toLowerCase()
        .includes(query.toLowerCase()),
  );
  const textMode = mode === "rename" || mode === "my_name";
  const memberMode = mode === "add_members" || mode === "remove_members";
  const blocked = !mode || mode === "result" || !enabled(mode) || refreshing;
  const canDismiss =
    operation.receipt?.notAdmitted ||
    (groupEnded(operation.receipt?.operation) &&
      receiptState !== "needs_review");
  return (
    <>
      <div className="detail-section group-settings">
        <div className="group-section-head">
          <div className="section-head">{t("Group settings", "群聊设置")}</div>
          <button
            className="text-button"
            disabled={refreshing || !state.connected}
            onClick={() => void refresh()}
          >
            {refreshing
              ? t("Refreshing…", "正在刷新…")
              : t("Refresh group", "刷新群聊")}
          </button>
        </div>
        {group?.isInGroup === false ? (
          <p className="group-membership">
            {t("You are no longer in this group.", "你已不在该群聊中。")}
          </p>
        ) : (
          group?.isInGroup !== true && (
            <p className="field-help">
              {t(
                "Membership is unconfirmed. Refresh before making changes.",
                "群成员身份尚未确认，修改前请先刷新。",
              )}
            </p>
          )
        )}
        <div className="group-setting-row">
          <span>
            {t("Group name", "群名称")}
            <strong>
              {group?.name ||
                group?.displayName ||
                t("Unnamed group", "未命名群聊")}
            </strong>
          </span>
          <button
            disabled={!enabled("rename") || refreshing}
            aria-label={t("Edit group name", "编辑群名称")}
            onClick={() => edit("rename")}
          >
            {t("Edit", "编辑")}
          </button>
        </div>
        <div className="group-setting-row">
          <span>
            {t("My name in this group", "我在本群的昵称")}
            <strong>
              {group?.myDisplayName || t("Using profile name", "使用个人昵称")}
            </strong>
          </span>
          <button
            disabled={!enabled("my_name") || refreshing}
            aria-label={t("Edit my name in this group", "编辑我在本群的昵称")}
            onClick={() => edit("my_name")}
          >
            {t("Edit", "编辑")}
          </button>
        </div>
        <div className="group-announcement">
          <div className="section-head">{t("Announcement", "群公告")}</div>
          <p>
            {group?.announcement === null || group?.announcement === undefined
              ? t("Not available in the saved view.", "保存的视图中暂不可用。")
              : group.announcement || t("No announcement.", "暂无群公告。")}
          </p>
          <button
            disabled={
              !enabled("clear_announcement") ||
              refreshing ||
              !group?.announcement
            }
            onClick={() => edit("clear_announcement")}
          >
            {t("Clear announcement", "清空群公告")}
          </button>
        </div>
        {operation.receipt && (
          <button
            className="secondary group-pending"
            onClick={() => setMode("result")}
          >
            {t("View last group change", "查看上次群聊变更")}
          </button>
        )}
        <button
          className="group-leave"
          disabled={!enabled("leave") || refreshing}
          onClick={() => edit("leave")}
        >
          {t("Leave group", "退出群聊")}
        </button>
        {error && (
          <p className="attachment-error" role="alert">
            {error}
          </p>
        )}
      </div>
      <div className="detail-section group-settings">
        <div className="group-section-head">
          <div className="section-head">
            {t("Members", "群成员")} <small>{members.length}</small>
          </div>
          <div className="group-member-actions">
            <button
              disabled={!enabled("add_members") || refreshing}
              onClick={() => edit("add_members")}
            >
              {t("Add", "添加")}
            </button>
            <button
              disabled={
                !enabled("remove_members") ||
                refreshing ||
                !members.some((member) => member.id !== own)
              }
              onClick={() => edit("remove_members")}
            >
              {t("Remove", "移除")}
            </button>
          </div>
        </div>
        {members.map((member) => (
          <div key={member.id} className="member">
            <HubAvatar
              connection={connection}
              account={account}
              username={member.username || member.id}
              name={member.displayName || member.id}
            />
            <span>
              {member.roomDisplayName || member.displayName || member.username}
              {member.id === own && <small> {t("(you)", "（你）")}</small>}
            </span>
          </div>
        ))}
      </div>
      {mode && (
        <div className="overlay-backdrop" onClick={() => setMode(null)}>
          <section
            className="modal group-edit"
            role="dialog"
            aria-modal="true"
            aria-label={
              mode === "result"
                ? t("Group change result", "群聊变更结果")
                : groupLabel(mode)
            }
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Escape") setMode(null);
            }}
          >
            <div className="modal-head">
              <h2>
                {mode === "result"
                  ? t("Group change result", "群聊变更结果")
                  : groupLabel(mode)}
              </h2>
              <Button
                icon="close"
                label={t("Close group settings", "关闭群聊设置")}
                onClick={() => setMode(null)}
              />
            </div>
            {operation.receipt ? (
              <>
                <GroupResult model={model} />
                <div className="group-edit-footer">
                  {!canDismiss && (
                    <button
                      className="secondary"
                      disabled={operation.busy}
                      onClick={() => void model.check()}
                    >
                      {t("Check status", "检查状态")}
                    </button>
                  )}
                  {operation.receipt.operation &&
                    !groupEnded(operation.receipt.operation) && (
                      <button
                        className="text-button"
                        disabled={operation.busy}
                        onClick={() => void model.cancel()}
                      >
                        {t("Cancel if not started", "若未开始则取消")}
                      </button>
                    )}
                  {canDismiss && (
                    <button
                      className="primary"
                      onClick={() => {
                        model.clear();
                        setMode(null);
                      }}
                    >
                      {t("Done", "完成")}
                    </button>
                  )}
                </div>
              </>
            ) : (
              <form
                onSubmit={(e) => {
                  e.preventDefault();
                  void submit();
                }}
              >
                {textMode && (
                  <label className="group-name">
                    {mode === "rename"
                      ? t("Group name", "群名称")
                      : t("My display name", "我的本群昵称")}
                    <input
                      autoFocus
                      aria-label={
                        mode === "rename"
                          ? t("Group name", "群名称")
                          : t("My display name", "我的本群昵称")
                      }
                      maxLength={512}
                      value={draft}
                      onChange={(e) => setDraft(e.target.value)}
                    />
                    <small className="field-help">
                      {mode === "rename"
                        ? t(
                            "Everyone in the group can see this name.",
                            "群内所有成员均可看到此名称。",
                          )
                        : t(
                            "Leave blank to use your profile name.",
                            "留空则使用你的个人昵称。",
                          )}
                    </small>
                  </label>
                )}
                {memberMode && (
                  <div className="member-picker">
                    <label className="search">
                      <input
                        autoFocus
                        aria-label={t("Find group contacts", "查找群联系人")}
                        placeholder={t("Find contacts", "查找联系人")}
                        value={query}
                        onChange={(e) => setQuery(e.target.value)}
                      />
                    </label>
                    <div className="new-chat-contacts">
                      {visible.map((item) => (
                        <label className="new-chat-contact" key={item.id}>
                          <HubAvatar
                            connection={connection}
                            account={account}
                            username={item.id}
                            name={item.displayName || item.id}
                          />
                          <span>{item.displayName || item.id}</span>
                          <input
                            type="checkbox"
                            aria-label={t(
                              `Select ${item.displayName || item.id}`,
                              `选择 ${item.displayName || item.id}`,
                            )}
                            checked={selected.includes(item.id)}
                            disabled={
                              !selected.includes(item.id) &&
                              selected.length >= 40
                            }
                            onChange={() =>
                              setSelected((current) =>
                                current.includes(item.id)
                                  ? current.filter((id) => id !== item.id)
                                  : [...current, item.id],
                              )
                            }
                          />
                        </label>
                      ))}
                      {!visible.length && (
                        <p className="no-results">
                          {t(
                            "No contacts available. Refresh the group or contact list.",
                            "暂无可选联系人，请刷新群聊或联系人列表。",
                          )}
                        </p>
                      )}
                    </div>
                    <p className="field-help member-picker-help">
                      {t(
                        `${selected.length} selected · WeChat checks who can be added or removed. Each member’s result will be shown.`,
                        `已选 ${selected.length} 人 · 微信将校验可添加或移除的成员，并显示每位成员的结果。`,
                      )}
                    </p>
                  </div>
                )}
                {mode === "leave" && (
                  <p>
                    {t("Leave ", "确定退出 ")}
                    <strong>{group?.displayName || groupId}</strong>
                    {t(
                      "? You’ll stop receiving new messages. Cached history will remain on this agent-wechat hub.",
                      "？你将不再接收新消息，已缓存的聊天记录仍保留在此 agent-wechat Hub 上。",
                    )}
                  </p>
                )}
                {mode === "clear_announcement" && (
                  <p>
                    {t(
                      "Clear the announcement for everyone in this group? WeChat checks your permission to make this change.",
                      "确定为群内所有人清空群公告？微信将校验你的修改权限。",
                    )}
                  </p>
                )}
                {mode === "mute" && (
                  <p>
                    {group?.muted === true
                      ? t("Turn notifications on for ", "开启通知：")
                      : t("Mute notifications for ", "静音通知：")}
                    <strong>{group?.displayName || groupId}</strong>
                    {t("?", "？")}
                  </p>
                )}
                <div className="group-edit-footer">
                  <button
                    type="button"
                    className="secondary"
                    onClick={() => setMode(null)}
                  >
                    {t("Cancel", "取消")}
                  </button>
                  <button
                    className={`primary ${
                      mode === "leave" || mode === "remove_members"
                        ? "danger"
                        : ""
                    }`}
                    disabled={
                      blocked ||
                      (mode === "rename" && !draft.trim()) ||
                      (textMode &&
                        new TextEncoder().encode(draft).length > 512) ||
                      (memberMode && !selected.length)
                    }
                  >
                    {mode === "leave"
                      ? t("Leave group", "退出群聊")
                      : mode === "clear_announcement"
                      ? t("Clear announcement", "清空群公告")
                      : memberMode
                      ? mode === "add_members"
                        ? t("Add members", "添加成员")
                        : t("Remove members", "移除成员")
                      : t("Save changes", "保存更改")}
                  </button>
                </div>
              </form>
            )}
            {connection.simulator && (
              <p className="field-help">
                {t(
                  "Simulator account · No real WeChat group is changed.",
                  "模拟器账号 · 不会修改真实的微信群聊。",
                )}
              </p>
            )}
          </section>
        </div>
      )}
    </>
  );
}
