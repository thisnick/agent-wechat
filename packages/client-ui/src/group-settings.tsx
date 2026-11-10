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
import { richText, t } from "./i18n.ts";
import { Button } from "./ui.tsx";
import { inventory, pause, readableError } from "./session.ts";

function groupLabel(action: GroupAction["action"]): string {
  switch (action) {
    case "create":
      return t("groupSettings.createGroup");
    case "rename":
      return t("groupSettings.renameGroup");
    case "leave":
      return t("groupSettings.leaveGroup");
    case "add_members":
      return t("groupSettings.addMembers");
    case "remove_members":
      return t("groupSettings.removeMembers");
    case "my_name":
      return t("groupSettings.myNameGroup");
    case "clear_announcement":
      return t("groupSettings.clearAnnouncement");
    case "mute":
      return t("groupSettings.notifications");
    case "pin":
      return t("groupSettings.pinChat");
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
          ? t("groupSettings.changeWasnTSubmitted")
          : operation?.state === "succeeded"
          ? partial
            ? receipt.input.action === "create"
              ? t("groupSettings.groupCreatedSomeSelectedContacts")
              : t("groupSettings.groupUpdatedSomeMemberChanges")
            : receipt.input.action === "leave"
            ? t("groupSettings.leftGroupCachedHistoryStill")
            : receipt.input.action === "create"
            ? t("groupSettings.groupCreated")
            : t("groupSettings.groupUpdated")
          : operation?.state === "needs_review"
          ? t("groupSettings.outcomeCouldnTConfirmedCheck")
          : operation?.state === "failed"
          ? t("groupSettings.changeFailedReviewResultBefore")
          : operation?.state === "cancelled"
          ? t("groupSettings.cancelledBeforeSubmission")
          : operation
          ? t("groupSettings.updatingGroup")
          : t("groupSettings.checkingWhetherChangeWasSubmitted")}
      </p>
      {result?.members && result.members.length > 0 && (
        <ul className="group-member-results">
          {result.members.map((member) => (
            <li key={member.username}>
              <span>{member.username}</span>
              <small>
                {receipt.input.action === "remove_members" &&
                member.status === null
                  ? t("groupSettings.removalAcknowledged")
                  : member.status === 0
                  ? t("groupSettings.accepted")
                  : t("groupSettings.notConfirmedStatus", { status: member.status ?? t("groupSettings.statusUnknown") })}
              </small>
            </li>
          ))}
        </ul>
      )}
      {!!result?.unconfirmedMembers?.length && (
        <p className="field-help">
          {t("groupSettings.unconfirmedTheseMembersWereNot", { name: result.unconfirmedMembers.join(", ") })}
        </p>
      )}
      {result?.steps?.some((step) => !step.accepted) && (
        <p className="field-help">
          {t("groupSettings.oneMoreStepsWereRejected")}
          {receipt.input.action === "leave"
            ? t("groupSettings.finalQuitResultShownAbove")
            : t("groupSettings.refreshGroupInspectCurrentState")}
        </p>
      )}
      {result?.remoteOutcome === "accepted" &&
        result.profileProjection === "pending" && (
          <p className="field-help">
            {t("groupSettings.wechatAcceptedChangeSavedView")}
          </p>
        )}
      {partial && (
        <p className="field-help">
          {t("groupSettings.noMembersWillRetriedAutomatically")}
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
  title,
}: {
  connection: Connection;
  account: string;
  groupId: string;
  title: string;
}) {
  const session = connection.session;
  const state = useSyncExternalStore(session.subscribe, session.snapshot);
  const model = connection.groupCreation(account, groupId);
  const operation = useSyncExternalStore(model.subscribe, model.snapshot);
  const [group, setGroup] = useState<NativeGroup | null>(null);
  const sharedName = group?.name === undefined
    ? (group?.displayName === groupId ? null : group?.displayName)
    : group.name;
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
          t("groupSettings.groupRefreshDidNotFinish"),
        );
      await pause(700, signal);
    }
    throw new Error(
      t("groupSettings.groupRefreshStillPendingTry"),
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
        ? sharedName || ""
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
          <div className="section-head">{t("groupSettings.groupSettings")}</div>
          <button
            className="text-button"
            disabled={refreshing || !state.connected}
            onClick={() => void refresh()}
          >
            {refreshing
              ? t("groupSettings.refreshing")
              : t("groupSettings.refreshGroup")}
          </button>
        </div>
        {group?.isInGroup === false ? (
          <p className="group-membership">
            {t("groupSettings.noLongerGroup")}
          </p>
        ) : (
          group?.isInGroup !== true && (
            <p className="field-help">
              {t("groupSettings.membershipUnconfirmedRefreshBeforeMaking")}
            </p>
          )
        )}
        <div className="group-setting-row">
          <span>
            {t("groupSettings.groupName")}
            <strong>
              {sharedName ||
                t("groupSettings.unnamedGroup")}
            </strong>
          </span>
          <button
            disabled={!enabled("rename") || refreshing}
            aria-label={t("groupSettings.editGroupName")}
            onClick={() => edit("rename")}
          >
            {t("groupSettings.edit")}
          </button>
        </div>
        <div className="group-setting-row">
          <span>
            {t("groupSettings.myNameGroup")}
            <strong>
              {group?.myDisplayName || t("groupSettings.usingProfileName")}
            </strong>
          </span>
          <button
            disabled={!enabled("my_name") || refreshing}
            aria-label={t("groupSettings.editMyNameGroup")}
            onClick={() => edit("my_name")}
          >
            {t("groupSettings.edit")}
          </button>
        </div>
        <div className="group-announcement">
          <div className="section-head">{t("groupSettings.announcement")}</div>
          <p>
            {group?.announcement === null || group?.announcement === undefined
              ? t("groupSettings.notAvailableSavedView")
              : group.announcement || t("groupSettings.noAnnouncement")}
          </p>
          <button
            disabled={
              !enabled("clear_announcement") ||
              refreshing ||
              !group?.announcement
            }
            onClick={() => edit("clear_announcement")}
          >
            {t("groupSettings.clearAnnouncement")}
          </button>
        </div>
        {operation.receipt && (
          <button
            className="secondary group-pending"
            onClick={() => setMode("result")}
          >
            {t("groupSettings.viewLastGroupChange")}
          </button>
        )}
        <button
          className="group-leave"
          disabled={!enabled("leave") || refreshing}
          onClick={() => edit("leave")}
        >
          {t("groupSettings.leaveGroup")}
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
            {t("groupSettings.members")} <small>{members.length}</small>
          </div>
          <div className="group-member-actions">
            <button
              disabled={!enabled("add_members") || refreshing}
              onClick={() => edit("add_members")}
            >
              {t("groupSettings.add")}
            </button>
            <button
              disabled={
                !enabled("remove_members") ||
                refreshing ||
                !members.some((member) => member.id !== own)
              }
              onClick={() => edit("remove_members")}
            >
              {t("groupSettings.remove")}
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
              {member.id === own && <small> {t("groupSettings.you")}</small>}
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
                ? t("groupSettings.groupChangeResult")
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
                  ? t("groupSettings.groupChangeResult")
                  : groupLabel(mode)}
              </h2>
              <Button
                icon="close"
                label={t("groupSettings.closeGroupSettings")}
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
                      {t("groupSettings.checkStatus")}
                    </button>
                  )}
                  {operation.receipt.operation &&
                    !groupEnded(operation.receipt.operation) && (
                      <button
                        className="text-button"
                        disabled={operation.busy}
                        onClick={() => void model.cancel()}
                      >
                        {t("groupSettings.cancelIfNotStarted")}
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
                      {t("groupSettings.done")}
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
                      ? t("groupSettings.groupName")
                      : t("groupSettings.myDisplayName")}
                    <input
                      autoFocus
                      aria-label={
                        mode === "rename"
                          ? t("groupSettings.groupName")
                          : t("groupSettings.myDisplayName")
                      }
                      maxLength={512}
                      value={draft}
                      onChange={(e) => setDraft(e.target.value)}
                    />
                    <small className="field-help">
                      {mode === "rename"
                        ? t("groupSettings.everyoneGroupCanSeeName")
                        : t("groupSettings.leaveBlankUseProfileName")}
                    </small>
                  </label>
                )}
                {memberMode && (
                  <div className="member-picker">
                    <label className="search">
                      <input
                        autoFocus
                        aria-label={t("groupSettings.findGroupContacts")}
                        placeholder={t("groupSettings.findContacts")}
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
                            aria-label={t("groupSettings.select", { value: item.displayName || item.id })}
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
                          {t("groupSettings.noContactsAvailableRefreshGroup")}
                        </p>
                      )}
                    </div>
                    <p className="field-help member-picker-help">
                      {t("groupSettings.selectedWechatChecksWhoCan", { count: selected.length })}
                    </p>
                  </div>
                )}
                {mode === "leave" && (
                  <p>
                    {richText("groupSettings.leaveConfirm", { title: <strong>{title}</strong> })}
                  </p>
                )}
                {mode === "clear_announcement" && (
                  <p>
                    {t("groupSettings.clearAnnouncementEveryoneGroupWechat")}
                  </p>
                )}
                {mode === "mute" && (
                  <p>
                    {richText(
                      group?.muted === true
                        ? "groupSettings.turnNotificationsOnFor"
                        : "groupSettings.muteNotificationsFor",
                      { title: <strong>{title}</strong> },
                    )}
                  </p>
                )}
                <div className="group-edit-footer">
                  <button
                    type="button"
                    className="secondary"
                    onClick={() => setMode(null)}
                  >
                    {t("groupSettings.cancel")}
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
                      ? t("groupSettings.leaveGroup")
                      : mode === "clear_announcement"
                      ? t("groupSettings.clearAnnouncement")
                      : memberMode
                      ? mode === "add_members"
                        ? t("groupSettings.addMembers")
                        : t("groupSettings.removeMembers")
                      : t("groupSettings.saveChanges")}
                  </button>
                </div>
              </form>
            )}
            {connection.simulator && (
              <p className="field-help">
                {t("groupSettings.simulatorAccountNoRealWechat")}
              </p>
            )}
          </section>
        </div>
      )}
    </>
  );
}
