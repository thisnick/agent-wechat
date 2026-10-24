import { useEffect, useRef, useState, useId } from "react";
import { createPortal } from "react-dom";
import { NativeApiError } from "@agent-wechat/shared";
import { Icon } from "./ui.tsx";
import { canRemoveProfile, profileRemovalName } from "./profile-removal.ts";
import type { Connection } from "./connection.ts";
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
      if (!lifetime.current.signal.aborted) setNotice("Account name saved.");
    } catch (error) {
      if (!lifetime.current.signal.aborted) setNotice(readableError(error));
    } finally {
      if (!lifetime.current.signal.aborted) setBusy(false);
    }
  }
  async function logout() {
    setBusy(true);
    setNotice("Signing out…");
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
            "Sign-out could not be fully confirmed. Check its status before trying again.",
          );
          return;
        }
        await pause(700, lifetime.current.signal);
      }
      if (!lifetime.current.signal.aborted) {
        setUncertain(true);
        setNotice("Sign-out is still pending. Check its status.");
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
    <section className="account-settings" aria-label="Account settings">
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void rename();
        }}
      >
        <label htmlFor="account-alias">Account name</label>
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
            Save
          </button>
        </div>
        <small>Letters, numbers, hyphens and underscores.</small>
      </form>
      {(account.authenticated || signOutRequired) &&
        (confirm ? (
          <div className="signout-confirm">
            <p>
              Sign out of <strong>{account.alias || account.id}</strong>? Chat
              history stays on this agent-wechat hub.
            </p>
            <div className="attachment-actions">
              <button
                className="secondary"
                disabled={busy}
                onClick={() => setConfirm(false)}
              >
                Keep signed in
              </button>
              <button
                className="primary"
                disabled={busy}
                onClick={() => void logout()}
              >
                {busy
                  ? "Signing out…"
                  : uncertain
                    ? "Check sign-out status"
                    : "Sign out"}
              </button>
            </div>
          </div>
        ) : (
          <button
            className="secondary signout-button"
            disabled={busy}
            onClick={() => setConfirm(true)}
          >
            Sign out of WeChat
          </button>
        ))}
      {!account.authenticated && signedIn && !removing && (
        <button className="primary" disabled={busy} onClick={signedIn}>
          Sign in to WeChat
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
            <h3>Remove {profileRemovalName(account)}?</h3>
            <p>
              This removes the profile from this agent-wechat hub. Local files
              are retained.
            </p>
            <label htmlFor="remove-profile-confirm">
              Type <strong>{profileRemovalName(account)}</strong> to confirm
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
                Cancel
              </button>
              <button
                className="danger"
                disabled={busy || !canRemoveProfile(account, confirmation)}
              >
                {busy ? "Removing…" : "Remove profile"}
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
              Remove profile
            </button>
            {(account.authenticated || signOutRequired) && (
              <small>Sign out before removing this profile.</small>
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
          <p>{account.authenticated ? "Signed in" : "Signed out"}</p>
        </div>
        <button
          autoFocus
          className="profile-close"
          aria-label="Close profile settings"
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
