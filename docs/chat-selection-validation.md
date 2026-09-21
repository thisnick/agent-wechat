# Verified chat selection

The selector now resolves a username against the native ordered vector during
the real UI click, waits for the native selector to return, and verifies the
current username. A hook firing is not success. An accessibility check must
also establish a responsive, open chat UI.

On ARM build `e9f1cd04`, calling the selector for an already-open chat toggles it
closed. Repeated selection therefore returns `skipped: true` when native identity
and open UI agree, including for legacy `--force`. The flag remains accepted;
it does not force an unnecessary click. A closed UI still requires selection.
An off-screen sidebar row is supported by checking the visible Messages pane
and Send button; display names are never used as account identity.

Frida output waits are bounded even on silent processes. Hooks are installed
before READY, cleaned up outside native callbacks, and serialized with a lock.
Each invocation uses a private temporary script. Ordered-map indices are not
renumbered across excluded entries; legacy ARM direct-vector behavior is retained.

## Validation, 2026-09-21

- 24 Python tests passed natively in both amd64 and ARM Docker, including a real
  SQLCipher fixture. Run `python3 -m unittest discover -s tests -v`.
- On amd64 `ce28c347`, three incoming test messages appeared in the UI; nine
  subsequent ordinary-chat selections matched independent native readback.
- OCI ARM `e9f1cd04` reproduced the forced-repeat toggle. After the guard was
  added, three incoming messages, their sender selections, and nine subsequent
  ordinary-chat selections passed. Off-screen forced repeat returned success
  without closing the chat.
- Missed clicks returned failure in about ten seconds on each architecture;
  subsequent valid selections recovered. Already-selected shortcuts passed.

Tests used isolated containers and fresh test-account logins. Production was
used only as an explicitly authorized sender; no production software was changed.
No credentials or private traces are included here.

## Limits

- The final open-pane/idempotence guard was live-tested on ARM; its expanded
  automated tests passed on both architectures. Earlier live amd64 tests preceded
  that final guard.
- These were direct-tool selection tests. The older research agent API rejected
  `wx chats open` as not logged in despite the WeChat client being logged in;
  matching-agent end-to-end API validation remains outstanding.
- The reported spontaneous UI freeze was not reproduced or claimed fixed.
- A deeper ARM diagnostic probe restarted the isolated client. That probe is
  not part of this change; final validation used entry/return hooks only.
- No download-trigger discovery, native CDN calls, key extraction changes,
  migrations, or build-offset changes are included in this PR.
