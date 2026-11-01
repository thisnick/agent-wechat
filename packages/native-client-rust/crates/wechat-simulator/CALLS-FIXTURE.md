# Calls API integration fixture

Use `wx-calls-fixture` to exercise the real Rust call API and ownership broker
from the TypeScript SDK, a passive CLI, or a plugin with a synthetic voice
provider. The executable creates its own temporary Store, account, and token
files, listens on an allocated loopback port, and removes the state on exit.

Build with the repository's pinned Rust toolchain:

```sh
cargo build --profile test --locked --manifest-path packages/native-client-rust/Cargo.toml -p wechat-simulator --features calls-fixture --bin wx-calls-fixture
```

Run the cross-language smoke from the repository root after building the shared
SDK:

```sh
pnpm --filter @agent-wechat/shared build
node --experimental-strip-types scripts/native-calls-api-smoke.mjs
```

`WX_CALLS_FIXTURE_BINARY` selects the fixture executable. An adjacent `wx` binary
adds the CLI checks; `WX_BINARY` selects it explicitly. The script starts and
stops its own fixture and uses a synthetic in-process voice provider.

Run the resulting `wx-calls-fixture` executable with no arguments. Its first
stdout line is JSON with `event: "ready"`, `baseUrl`, `accountId`, `tokenFile`,
`callsTokenFile`, and `nonCallsTokenFile`. Read a token from its private file and
send it in the Authorization header. `tokenFile` holds the owner test token;
`callsTokenFile` has only `calls` permission and supports passive observation and
participation. `nonCallsTokenFile` has only `read` permission and cannot use the
call routes.
Token contents and PCM samples are never printed by the fixture.

Each stdin line is JSON with a nonempty string `id` and an `op`. Responses echo
the ID and have `ok: true` with the requested metadata, or `ok: false` with a
fixed error `code`. Lines are limited to 16 KiB. For example:

```json
{"id":"offer-one","op":"offer","callerId":"synthetic-peer"}
{"id":"audio-one","op":"inject_audio","value":1234,"frames":2}
{"id":"state-one","op":"stats"}
{"id":"stop-one","op":"shutdown"}
```

| Operation | Behavior |
| --- | --- |
| `offer` | Supplies a trusted, already-ACKed synthetic direct voice offer; returns `callId` and `revision`. |
| `inject_audio` | Sends 1–4 generated 320-sample receive frames after a call connects. `value` is a signed 16-bit constant. |
| `end` | Supplies a trusted terminal result for the active synthetic call. |
| `stats` | Returns additive operation/audio counters, the last send generation/peak, and whether the backend has an active call. |
| `hold_clear` | Sets `enabled: true` to hold the fake backend's clear operation. |
| `release_clear` | Releases held clear operations while the API request is waiting. |
| `hold_end` | Sets `enabled: true` to delay terminal backend completion after an accepted cancel/hangup. |
| `release_end` | Releases held terminal completion while stdin and the authenticated call sockets remain available. |
| `shutdown` | Ends the synthetic session and stops the listener. EOF also stops it. |

Accepted dial/answer operations connect immediately. Accepted transmit PCM is
echoed to the receive track, so a test can check the actual binary frame path.
The fake backend reports terminal completion for reject, cancel, hangup, and
stop. It performs no remote operation.

With the printed values, the actual CLI can observe the same broker:

```sh
wx --server "$baseUrl" --account "$accountId" --token-file "$callsTokenFile" calls status
wx --server "$baseUrl" --account "$accountId" --token-file "$callsTokenFile" calls monitor --json
```

Give automation a private client configuration directory when launching the
CLI. These commands do not open audio devices or register answer readiness.

This fixture validates API authentication, client arbitration, media framing,
and client teardown across languages. Incoming notification staging, ringing
ACKs, native signaling, encryption, Opus, microphone/speaker devices, and peer
playback require their separate tests. No native upstream or account worker is
started; unrelated native operations fail through the simulator adapter.
