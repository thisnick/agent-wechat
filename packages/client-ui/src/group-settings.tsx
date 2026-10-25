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
import { Button } from "./ui.tsx";
import { inventory, pause, readableError } from "./session.ts";

const labels: Record<GroupAction["action"], string> = {
  create: "Create group",
  rename: "Rename group",
  leave: "Leave group",
  add_members: "Add members",
  remove_members: "Remove members",
  my_name: "My name in this group",
  clear_announcement: "Clear announcement",
  mute: "Notifications",
};
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
  if (!receipt) return null;
  const operation = receipt.operation;
  const result = operation?.result as Result | undefined;
  const partial = result?.memberOutcome === "partial";
  return (
    <div className="group-operation-result">
      <p role="status">
        {receipt.notAdmitted
          ? "The change wasn’t submitted."
          : operation?.state === "succeeded"
            ? partial
              ? receipt.input.action === "create"
                ? "Group created. Some selected contacts weren’t confirmed as added."
                : "Group updated. Some member changes weren’t confirmed."
              : receipt.input.action === "leave"
                ? "You left the group. Cached history is still available."
                : receipt.input.action === "create"
                  ? "Group created."
                  : "Group updated."
            : operation?.state === "needs_review"
              ? "The outcome couldn’t be confirmed. Check the group before attempting this change again."
              : operation?.state === "failed"
                ? "The change failed. Review the result before trying again."
                : operation?.state === "cancelled"
                  ? "Cancelled before submission."
                  : operation
                    ? "Updating group…"
                    : "Checking whether the change was submitted…"}
      </p>
      {result?.members && result.members.length > 0 && (
        <ul className="group-member-results">
          {result.members.map((member) => (
            <li key={member.username}>
              <span>{member.username}</span>
              <small>
                {receipt.input.action === "remove_members" &&
                member.status === null
                  ? "Removal acknowledged"
                  : member.status === 0
                    ? "Accepted"
                    : `Not confirmed (status ${member.status ?? "unknown"})`}
              </small>
            </li>
          ))}
        </ul>
      )}
      {!!result?.unconfirmedMembers?.length && (
        <p className="field-help">
          Unconfirmed: {result.unconfirmedMembers.join(", ")}. These members
          were not assumed to have changed.
        </p>
      )}
      {result?.steps?.some((step) => !step.accepted) && (
        <p className="field-help">
          One or more steps were rejected.{" "}
          {receipt.input.action === "leave"
            ? "The final quit result is shown above; cleanup results are retained separately."
            : "Refresh the group to inspect its current state."}
        </p>
      )}
      {result?.remoteOutcome === "accepted" &&
        result.profileProjection === "pending" && (
          <p className="field-help">
            WeChat accepted this change. The saved view on this agent-wechat hub
            is still updating.
          </p>
        )}
      {partial && (
        <p className="field-help">No members will be retried automatically.</p>
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
  }, [load, state.threads, state.contacts]);
  const receiptState = operation.receipt?.operation?.state;
  useEffect(() => {
    if (receiptState && groupEnded(operation.receipt?.operation)) {
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
          "The group refresh did not finish. The saved view may be incomplete.",
        );
      await pause(700, signal);
    }
    throw new Error(
      "Group refresh is still pending. Try Refresh group again to inspect the latest view.",
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
    if (operation.receipt) {
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
    if (!mode || mode === "result" || mode === "create") return;
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
          <div className="section-head">Group settings</div>
          <button
            className="text-button"
            disabled={refreshing || !state.connected}
            onClick={() => void refresh()}
          >
            {refreshing ? "Refreshing…" : "Refresh group"}
          </button>
        </div>
        {group?.isInGroup === false ? (
          <p className="group-membership">You are no longer in this group.</p>
        ) : (
          group?.isInGroup !== true && (
            <p className="field-help">
              Membership is unconfirmed. Refresh before making changes.
            </p>
          )
        )}
        <div className="group-setting-row">
          <span>
            Group name
            <strong>
              {group?.name || group?.displayName || "Unnamed group"}
            </strong>
          </span>
          <button
            disabled={!enabled("rename") || refreshing}
            aria-label="Edit group name"
            onClick={() => edit("rename")}
          >
            Edit
          </button>
        </div>
        <div className="group-setting-row">
          <span>
            My name in this group
            <strong>{group?.myDisplayName || "Using profile name"}</strong>
          </span>
          <button
            disabled={!enabled("my_name") || refreshing}
            aria-label="Edit my name in this group"
            onClick={() => edit("my_name")}
          >
            Edit
          </button>
        </div>
        <button
          className="group-toggle"
          role="switch"
          aria-checked={group?.muted === true}
          disabled={!enabled("mute") || refreshing}
          onClick={() => edit("mute")}
        >
          <span>
            Mute notifications
            <small>
              {group?.muted === true
                ? "On"
                : group?.muted === false
                  ? "Off"
                  : "Unknown"}
            </small>
          </span>
          <span className={`toggle-track ${group?.muted ? "on" : ""}`}>
            <i />
          </span>
        </button>
        <div className="group-announcement">
          <div className="section-head">Announcement</div>
          <p>
            {group?.announcement === null || group?.announcement === undefined
              ? "Not available in the saved view."
              : group.announcement || "No announcement."}
          </p>
          <button
            disabled={
              !enabled("clear_announcement") ||
              refreshing ||
              !group?.announcement
            }
            onClick={() => edit("clear_announcement")}
          >
            Clear announcement
          </button>
        </div>
        {operation.receipt && (
          <button
            className="secondary group-pending"
            onClick={() => setMode("result")}
          >
            View last group change
          </button>
        )}
        <button
          className="group-leave"
          disabled={!enabled("leave") || refreshing}
          onClick={() => edit("leave")}
        >
          Leave group
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
            Members <small>{members.length}</small>
          </div>
          <div className="group-member-actions">
            <button
              disabled={!enabled("add_members") || refreshing}
              onClick={() => edit("add_members")}
            >
              Add
            </button>
            <button
              disabled={
                !enabled("remove_members") ||
                refreshing ||
                !members.some((member) => member.id !== own)
              }
              onClick={() => edit("remove_members")}
            >
              Remove
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
              {member.id === own && <small> (you)</small>}
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
              mode === "result" ? "Group change result" : labels[mode]
            }
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (e.key === "Escape") setMode(null);
            }}
          >
            <div className="modal-head">
              <h2>
                {mode === "result" ? "Group change result" : labels[mode]}
              </h2>
              <Button
                icon="close"
                label="Close group settings"
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
                      Check status
                    </button>
                  )}
                  {operation.receipt.operation &&
                    !groupEnded(operation.receipt.operation) && (
                      <button
                        className="text-button"
                        disabled={operation.busy}
                        onClick={() => void model.cancel()}
                      >
                        Cancel if not started
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
                      Done
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
                    {mode === "rename" ? "Group name" : "My display name"}
                    <input
                      autoFocus
                      aria-label={
                        mode === "rename" ? "Group name" : "My display name"
                      }
                      maxLength={512}
                      value={draft}
                      onChange={(e) => setDraft(e.target.value)}
                    />
                    <small className="field-help">
                      {mode === "rename"
                        ? "Everyone in the group can see this name."
                        : "Leave blank to use your profile name."}
                    </small>
                  </label>
                )}
                {memberMode && (
                  <div className="member-picker">
                    <label className="search">
                      <input
                        autoFocus
                        aria-label="Find group contacts"
                        placeholder="Find contacts"
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
                            aria-label={`Select ${item.displayName || item.id}`}
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
                          No contacts available. Refresh the group or contact
                          list.
                        </p>
                      )}
                    </div>
                    <p className="field-help member-picker-help">
                      {selected.length} selected · WeChat checks who can be
                      added or removed. Each member’s result will be shown.
                    </p>
                  </div>
                )}
                {mode === "leave" && (
                  <p>
                    Leave <strong>{group?.displayName || groupId}</strong>?
                    You’ll stop receiving new messages. Cached history will
                    remain on this agent-wechat hub.
                  </p>
                )}
                {mode === "clear_announcement" && (
                  <p>
                    Clear the announcement for everyone in this group? WeChat
                    checks your permission to make this change.
                  </p>
                )}
                {mode === "mute" && (
                  <p>
                    {group?.muted === true
                      ? "Turn notifications on"
                      : "Mute notifications"}{" "}
                    for <strong>{group?.displayName || groupId}</strong>?
                  </p>
                )}
                <div className="group-edit-footer">
                  <button
                    type="button"
                    className="secondary"
                    onClick={() => setMode(null)}
                  >
                    Cancel
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
                      ? "Leave group"
                      : mode === "clear_announcement"
                        ? "Clear announcement"
                        : memberMode
                          ? mode === "add_members"
                            ? "Add members"
                            : "Remove members"
                          : "Save changes"}
                  </button>
                </div>
              </form>
            )}
            {connection.simulator && (
              <p className="field-help">
                Simulator account · No real WeChat group is changed.
              </p>
            )}
          </section>
        </div>
      )}
    </>
  );
}
