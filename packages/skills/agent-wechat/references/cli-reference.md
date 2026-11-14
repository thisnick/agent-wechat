# Discover and use the installed CLI

The installed command's `--help` describes its accepted flags. This reference shows common workflows rather than copying the entire command tree. In a source checkout, `interfaces/cli/wx.txt` is the generated command and flag inventory.

## Read account and saved data

```sh
wx --json accounts list
wx --json --account ACCOUNT accounts status
wx --json --account ACCOUNT accounts capabilities
wx --json --account ACCOUNT contacts find "Mira"
wx --json --account ACCOUNT contacts get CONTACT_ID
wx --json --account ACCOUNT chats list --limit 20
wx --json --account ACCOUNT messages list THREAD_ID --limit 20
```

Use `nextCursor` unchanged with the same query when paging. A nickname, remark, and contact ID may help resolve a person; do not assume the first search result is the intended recipient. Contact lookup and sync request information from WeChat; use those when saved information is insufficient and the request authorizes it.

## Submit one intent

```sh
wx --json --account ACCOUNT messages send THREAD_ID --text "Report ready" --idempotency-key INTENT_KEY
wx --json --account ACCOUNT messages send THREAD_ID --file "./report with spaces.pdf" --idempotency-key FILE_INTENT_KEY
wx --json --account ACCOUNT messages send THREAD_ID --image "./photo.jpg" --idempotency-key IMAGE_INTENT_KEY
wx --json --account ACCOUNT messages send THREAD_ID --video "./clip.mp4" --video-quality normal --idempotency-key VIDEO_INTENT_KEY
wx --json --account ACCOUNT messages send THREAD_ID --voice "./note.wav" --idempotency-key VOICE_INTENT_KEY
```

Use the supported audio formats described by the installed CLI; voice preparation produces the WeChat encoding. Inspect account capabilities and per-kind limits rather than assuming one file-size limit.

A quote uses both `--quote-message-id` and `--quote-server-id` from the selected message. A recall takes the opaque local message ID and exact remote ID:

```sh
wx --json --account ACCOUNT messages recall MESSAGE_ID --server-id SERVER_ID --idempotency-key RECALL_INTENT_KEY
```

WeChat decides eligibility. Keep partial and uncertain results; do not try another action or recipient as a workaround.

## Retrieve media

```sh
wx --json --account ACCOUNT messages media THREAD_ID MESSAGE_ID --quality best --output "./downloaded.jpg"
```

Choose a truthful output filename for the selected media. `--cached-only` reads an already saved copy without starting acquisition. Otherwise the command may create a download job. The output path is on the CLI host.

## Inspect asynchronous work

```sh
wx --json --account ACCOUNT jobs get JOB_ID
wx --json --account ACCOUNT operations get OPERATION_ID
wx --json --account ACCOUNT messages send-status SEND_ID
```

Use the identifier and kind actually returned. CLI parser/transport failures emit an `error` object on stderr; successful and pending result objects are on stdout. Do not expect every command to return the same fields. See [exit codes](exit-codes.md).
