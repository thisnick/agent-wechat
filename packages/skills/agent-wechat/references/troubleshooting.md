# Diagnose the configured client

Start with read-only inspection:

```sh
wx --version
wx --json config show
wx --json status
wx --json accounts list
wx --json --account ACCOUNT accounts status
```

Do not publish token values, QR secrets, cookies, private message bodies, or profile keys in logs or reports.

| Symptom | Investigate |
| --- | --- |
| Missing or unrelated `wx` | Inspect `wx --help`; use the user's configured native binary or the optional helper |
| Endpoint unavailable | Inspect the selected server and its configured endpoint; the daemon may be remote |
| Access denied | Check account/token scope without issuing a broader key |
| Login required | Use the requested account's login workflow and phone approval |
| Profile owned or locked | Identify the specific profile owner; preserve files and locks |
| Wait ended or `needs_review` | Inspect the existing job or send receipt, preserving its key |

`scripts/wx-helper.sh --doctor` checks native CLI availability/version and asks the configured hub for status. It does not test WeChat login, open raw gateway connections, install software, stop processes, change proxy settings, or start a daemon.

When managing the local daemon is part of the request, use `wx daemon status`, `wx daemon logs` and the appropriate documented lifecycle command. A local service check is not a status check for a remote hub.

Profile takeover is a separate action. An official application can remain running after its window closes; verify exclusive ownership before using `--official` or asserting `--source-stopped`. Do not delete a lock or force-kill all matching processes to bypass admission.

For network failures, inspect the configured transport and environment. Preserve the user's proxy/network configuration unless the requested change and its effect are understood.
