import { useEffect, useRef, useState, useId } from "react";
import { createPortal } from "react-dom";
import { NativeApiError } from "@agent-wechat/shared";
import { MediaDownloadSettings } from "./media-download-settings.tsx";
import { AccountNotifications } from "./account-notifications.tsx";
import { Icon } from "./ui.tsx";
import { canRemoveProfile, profileRemovalName } from "./profile-removal.ts";
import type { Connection } from "./connection.ts";
import { t, useLocale } from "./i18n.ts";
import type { Account } from "./session.ts";
import { pause, readableError } from "./session.ts";

export function AccountSettings({
  connection,
  account,
  signedOut,
  removed,
  signedIn,
}: {
  connection: Connection;
  account: Account;
  signedOut: () => void;
  removed: () => void;
  signedIn?: () => void;
}) {
  useLocale();
  const [alias, setAlias] = useState(account.alias || account.id);
  const [confirm, setConfirm] = useState(false),
    [busy, setBusy] = useState(false),
    [notice, setNotice] = useState("");
  const [removing, setRemoving] = useState(false);
  const [signOutRequired, setSignOutRequired] = useState(false);
  const [confirmation, setConfirmation] = useState("");
  const [uncertain, setUncertain] = useState(false);
  const lifetime = useRef(new AbortController());
  useEffect(() => () => lifetime.current.abort(), []);
  async function remove() {
    if (busy || !canRemoveProfile(account, confirmation)) return;
    setBusy(true);
    setNotice("");
    try {
      await connection.session.client.removeAccount(account.id, account.id);
      connection.incomingMedia.clear(account.id);
      connection.avatars.clear(account.id);
      connection.session.deactivate(account.id);
      await connection.session.accounts();
      if (!lifetime.current.signal.aborted) removed();
    } catch (error) {
      if (!lifetime.current.signal.aborted) {
        if (
          error instanceof NativeApiError &&
          error.code === "account_sign_out_required"
        ) {
          // Saved credentials can exist even when this daemon has not validated
          // a connection yet. Offer sign-out before retrying removal.
          setSignOutRequired(true);
          setRemoving(false);
          setConfirmation("");
        }
        setNotice(readableError(error));
      }
    } finally {
      if (!lifetime.current.signal.aborted) setBusy(false);
    }
  }
  async function rename() {
    setBusy(true);
    setNotice("");
    try {
      await connection.session.client.updateAccount(account.id, {
        alias: alias.trim(),
      });
      await connection.session.accounts();
      if (!lifetime.current.signal.aborted)
        setNotice(t("Account name saved.", "账号名称已保存。"));
    } catch (error) {
      if (!lifetime.current.signal.aborted) setNotice(readableError(error));
    } finally {
      if (!lifetime.current.signal.aborted) setBusy(false);
    }
  }
  async function logout() {
    setBusy(true);
    setNotice(t("Signing out…", "正在退出登录…"));
    try {
      const operation = await connection.logout(account.id);
      const deadline = Date.now() + 90000;
      while (!lifetime.current.signal.aborted && Date.now() < deadline) {
        const current = await connection.session.client.operation(
          account.id,
          operation.id,
          lifetime.current.signal,
        );
        if (current.state === "succeeded") {
          connection.incomingMedia.clear(account.id);
          connection.avatars.clear(account.id);
          await connection.session.accounts();
          if (!lifetime.current.signal.aborted) signedOut();
          return;
        }
        if (
          ["failed", "cancelled", "rejected", "needs_review"].includes(
            current.state,
          )
        ) {
          setUncertain(true);
          setNotice(
            t(
              "Sign-out could not be fully confirmed. Check its status before trying again.",
              "退出登录未能完全确认，请先检查状态再重试。",
            ),
          );
          return;
        }
        await pause(700, lifetime.current.signal);
      }
      if (!lifetime.current.signal.aborted) {
        setUncertain(true);
        setNotice(
          t(
            "Sign-out is still pending. Check its status.",
            "退出登录仍在进行中，请检查状态。",
          ),
        );
      }
    } catch (error) {
      if (!lifetime.current.signal.aborted) {
        setUncertain(true);
        setNotice(readableError(error));
      }
    } finally {
      if (!lifetime.current.signal.aborted) setBusy(false);
    }
  }
  return (
    <section
      className="account-settings"
      aria-label={t("Account settings", "账号设置")}
    >
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void rename();
        }}
      >
        <label htmlFor="account-alias">{t("Account name", "账号名称")}</label>
        <div className="account-name">
          <input
            id="account-alias"
            value={alias}
            onChange={(event) => setAlias(event.target.value)}
            maxLength={64}
            pattern="[A-Za-z0-9_-]+"
            required
            disabled={busy}
          />
          <button
            className="secondary"
            disabled={busy || alias.trim() === (account.alias || account.id)}
          >
            {t("Save", "保存")}
          </button>
        </div>
        <small>
          {t(
            "Letters, numbers, hyphens and underscores.",
            "仅支持字母、数字、连字符和下划线。",
          )}
        </small>
      </form>
      <MediaDownloadSettings connection={connection} account={account.id} />
      <AccountNotifications key={JSON.stringify([connection.desktopId, connection.cache.namespace[1], account.id])} connection={connection} account={account.id} />
      {(account.authenticated || signOutRequired) &&
        (confirm ? (
          <div className="signout-confirm">
            <p>
              {t("Sign out of ", "确定退出 ")}
              <strong>{account.alias || account.id}</strong>
              {t(
                "? Chat history stays on this agent-wechat hub.",
                "？聊天记录将保留在此 agent-wechat Hub 上。",
              )}
            </p>
            <div className="attachment-actions">
              <button
                className="secondary"
                disabled={busy}
                onClick={() => setConfirm(false)}
              >
                {t("Keep signed in", "保持登录")}
              </button>
              <button
                className="primary"
                disabled={busy}
                onClick={() => void logout()}
              >
                {busy
                  ? t("Signing out…", "正在退出登录…")
                  : uncertain
                  ? t("Check sign-out status", "检查退出状态")
                  : t("Sign out", "退出登录")}
              </button>
            </div>
          </div>
        ) : (
          <button
            className="secondary signout-button"
            disabled={busy}
            onClick={() => setConfirm(true)}
          >
            {t("Sign out of WeChat", "退出微信登录")}
          </button>
        ))}
      {!account.authenticated && signedIn && !removing && (
        <button className="primary" disabled={busy} onClick={signedIn}>
          {t("Sign in to WeChat", "登录微信")}
        </button>
      )}
      <div className="profile-removal">
        {removing ? (
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void remove();
            }}
          >
            <h3>
              {t(
                `Remove ${profileRemovalName(account)}?`,
                `移除 ${profileRemovalName(account)}？`,
              )}
            </h3>
            <p>
              {t(
                "This removes the profile from this agent-wechat hub. Local files are retained.",
                "这将从此 agent-wechat Hub 移除该配置，本地文件将保留。",
              )}
            </p>
            <label htmlFor="remove-profile-confirm">
              {t("Type ", "输入 ")}
              <strong>{profileRemovalName(account)}</strong>
              {t(" to confirm", " 以确认")}
            </label>
            <input
              id="remove-profile-confirm"
              autoFocus
              autoComplete="off"
              spellCheck={false}
              value={confirmation}
              disabled={busy}
              onChange={(event) => setConfirmation(event.target.value)}
            />
            <div className="recall-confirm-actions">
              <button
                type="button"
                className="secondary"
                disabled={busy}
                onClick={() => {
                  setRemoving(false);
                  setConfirmation("");
                }}
              >
                {t("Cancel", "取消")}
              </button>
              <button
                className="danger"
                disabled={busy || !canRemoveProfile(account, confirmation)}
              >
                {busy
                  ? t("Removing…", "正在移除…")
                  : t("Remove profile", "移除配置")}
              </button>
            </div>
          </form>
        ) : (
          <>
            <button
              className="danger-text"
              disabled={busy || account.authenticated || signOutRequired}
              onClick={() => {
                setRemoving(true);
                setConfirmation("");
                setNotice("");
              }}
            >
              {t("Remove profile", "移除配置")}
            </button>
            {(account.authenticated || signOutRequired) && (
              <small>
                {t(
                  "Sign out before removing this profile.",
                  "移除此配置前请先退出登录。",
                )}
              </small>
            )}
          </>
        )}
      </div>
      {notice && (
        <p className="account-notice" role="status">
          {notice}
        </p>
      )}
    </section>
  );
}

export function ProfileDialog({
  connection,
  account,
  close,
  signedOut,
  removed,
  signedIn,
}: {
  connection: Connection;
  account: Account;
  close: () => void;
  signedOut: () => void;
  removed: () => void;
  signedIn: () => void;
}) {
  useLocale();
  const dialog = useRef<HTMLDialogElement>(null);
  const title = useId();
  useEffect(() => {
    const target = dialog.current;
    const focus = document.activeElement;
    target?.showModal();
    return () => {
      target?.close();
      if (focus instanceof HTMLElement && focus.isConnected) focus.focus();
    };
  }, []);
  return createPortal(
    <dialog
      ref={dialog}
      className="modal recall-confirm profile-dialog"
      aria-labelledby={title}
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
    >
      <header>
        <div>
          <h2 id={title}>{profileRemovalName(account)}</h2>
          <p>
            {account.authenticated
              ? t("Signed in", "已登录")
              : t("Signed out", "已退出")}
          </p>
        </div>
        <button
          autoFocus
          className="profile-close"
          aria-label={t("Close profile settings", "关闭配置设置")}
          onClick={close}
        >
          <Icon name="close" />
        </button>
      </header>
      <AccountSettings
        connection={connection}
        account={account}
        signedOut={signedOut}
        removed={removed}
        signedIn={signedIn}
      />
    </dialog>,
    document.body,
  );
}
