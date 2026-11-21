# Task workflows

Replace placeholders with IDs returned by the selected account. The examples are not instructions to send test messages.

## Resolve and send

1. Read `wx --json accounts list` and `wx --json --account ACCOUNT contacts find "Mira"`.
2. Resolve the intended recipient from the returned IDs and saved details. Clarify a genuine ambiguity; a unique intended contact does not need a new authorization question when the user already requested the send.
3. Submit the requested content once with a task-specific idempotency key:
   ```sh
   wx --json --account ACCOUNT messages send CONTACT_ID --text "Report ready" --idempotency-key INTENT_KEY
   ```
4. Inspect the actual result. Keep the ID and key. Use `messages send-status` for a text send or `jobs get` for a media job. A `needs_review` result ends submission, not the need to report what happened.

For files, use the path on the machine running `wx`. The CLI uploads its bytes to the selected daemon; a gateway-local filename is not automatically accessible to remote execution.

## Requested QR login

1. Inspect the account's current state first. Preserve an already working session.
2. When login is requested, start one attempt:
   ```sh
   wx --json --account ACCOUNT auth login --strategy qr --detach
   ```
3. Read the attempt ID from the returned object. Retrieve its QR image with `wx --json --account ACCOUNT auth qr ATTEMPT_ID --output "./login-qr.png"` and present it through the agent's supported image mechanism.
4. Ask the user to scan and approve when needed. Inspect the same attempt with `wx --json --account ACCOUNT auth operation ATTEMPT_ID`. Use its actual `state` and result; do not loop forever or treat QR creation as successful login.

For saved-device or official-profile login, inspect `wx auth login --help`. Official profile paths belong to the daemon host and require exclusive ownership. These flows do not guarantee that phone approval or a QR code can be skipped.

## Download a received attachment

1. Read the selected chat's messages and identify the intended message and attachment.
2. Inspect the command's supported qualities with `wx messages media --help`.
3. Retrieve the selected copy:
   ```sh
   wx --json --account ACCOUNT messages media THREAD_ID MESSAGE_ID --quality best --output "./chosen-file"
   ```
4. If a job is returned or the wait ends, inspect that job. Report a local output path only after the file is actually written. Preserve the delivered format and choose a matching extension.

## Requested group changes

Resolve the exact group and member IDs using `wx groups --help` and `wx groups members --help`. Inspect current membership before a change. Submit only the requested member set and topic with one intent key. A partial result, invitation outcome, or `needs_review` is not permission to create another group, change participants, or retry the mutation.

For token administration and profile maintenance, use the installed command help and the user's explicit requested scope. Do not search for an owner's secret or broaden a key to make an unrelated task work.
