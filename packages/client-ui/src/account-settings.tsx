import { useEffect, useRef, useState } from "react";
import type { Connection } from "./connection.ts";
import type { Account } from "./session.ts";
import { pause, readableError } from "./session.ts";

export function AccountSettings({
  connection,
  account,
  signedOut,
}: {
  connection: Connection;
  account: Account;
  signedOut: () => void;
}) {
  const [alias, setAlias] = useState(account.alias || account.id);
  const [confirm, setConfirm] = useState(false),
    [busy, setBusy] = useState(false),
    [notice, setNotice] = useState("");
  const [uncertain, setUncertain] = useState(false);
  const lifetime = useRef(new AbortController());
  useEffect(() => () => lifetime.current.abort(), []);
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
      {confirm ? (
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
      )}
      {notice && (
        <p className="account-notice" role="status">
          {notice}
        </p>
      )}
    </section>
  );
}
