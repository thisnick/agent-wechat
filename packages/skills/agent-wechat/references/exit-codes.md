# Exit codes and existing operations

Inspect the returned object as well as the process exit code. Zero also covers successfully queued work.

| Exit | Meaning | Next step |
| --- | --- | --- |
| 0 | The command returned a result; work may still be pending | Inspect `state` and any returned ID |
| 1 | Other operation failure | Read the error and existing operation |
| 2 | Invalid arguments, configuration, or account selection | Correct the input or select the intended account |
| 3 | The configured endpoint is unavailable | Check routing/connectivity; preserve any submitted intent key |
| 4 | Access denied | Use appropriately authorized existing credentials |
| 5 | Account needs login | Start login only when requested or authorized |
| 6 | Phone approval or another challenge is required | Present the challenge to the user |
| 7 | The client's wait ended | Inspect the existing job; daemon work continues |
| 8 | Outcome is unknown or needs review | Stop submission and inspect the existing operation |
| 9 | Failed, rejected, cancelled, expired, unavailable, unsupported, or not cached | Inspect the specific state and reason |
| 130 | Client wait interrupted | Work may still be running; inspect its ID |

These values come from the native CLI outcome handling. An endpoint error during a send does not establish that nothing was submitted.

## JSON

With `--json`, stdout contains the actual result object. It is not wrapped in a generic `outcome/data` envelope. Lists expose `items`/`nextCursor`; jobs and operations expose their own IDs and states.

Parser and transport failures write JSON to stderr with `error.code`, `error.message`, and `error.retryable`. A returned failed job can instead be a normal stdout result with a nonzero exit code. Capture both channels.

## Recovery

For a media job, inspect `wx --json --account ACCOUNT jobs get JOB_ID`. For account/group work use `operations get`; for a text-send receipt use `messages send-status`. Keep the original idempotency key, parameters, and returned IDs.

Do not assume that a sync or a matching message in history proves an uncertain intent was accepted. Do not create another key or resubmit because polling or the client's wait ended. If no operation ID was returned, report the original key and failure for reconciliation instead of inventing a lookup command.

Local writeback repair uses already owned bytes and receipts; it does not resend messages. Retry only when the operation is eligible and the requested recovery is understood. See the installed `wx jobs retry --help` and the product documentation.
