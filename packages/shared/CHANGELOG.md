# @agent-wechat/shared

## 1.0.0-beta.1

### Patch Changes

- [#223](https://github.com/thisnick/agent-wechat/pull/223) [`700f2c1`](https://github.com/thisnick/agent-wechat/commit/700f2c132eecd0fe16a80b5fee10efd1d2c3fdb9) Thanks [@thisnick](https://github.com/thisnick)! - Make whole-thread marking the only public read operation. Remove message-selection flags and request fields, keep expected-version checks, and mark a thread after confirmed OpenClaw model handoff unless reads are disabled. Add durable native cross-device read notifications that retain uncertain outcomes without automatic resubmission. Apply recognized incoming unread-clear controls while retaining unsupported session-exit behavior.

## 1.0.0-beta.0

### Major Changes

- [#215](https://github.com/thisnick/agent-wechat/pull/215) [`1e33596`](https://github.com/thisnick/agent-wechat/commit/1e335969911474870764926cf30f8c105c91107c) Thanks [@thisnick](https://github.com/thisnick)! - Replace the official-client container backend with the standalone native WeChat daemon and Rust `wx` CLI. This is a breaking release: remove `wx-legacy`, the UI server/image and Wechaty integrations; use native v1 HTTP/SSE and managed or external OpenClaw hosting. Existing container API configurations must be migrated explicitly.

  Take over a stopped, supported official Linux profile in place with `auth login --official[=/path]`, preserving account/device/database identity and saved-login material. Continue supported encrypted database, local read-state and media writes with durable sync, idempotent text/media jobs, independently encoded voice and explicit uncertainty handling. Add offline inspection, metadata catalogs and generated API contracts. Fresh-profile creation and copy import/export remain experimental extras.

  Distribute verified Linux x64 and ARM64 GNU executables (glibc 2.36+), an npm launcher and a native-only multi-architecture container. Add CI package/runtime tests, exact-source publication gates, codec notices and optional service/Compose examples. Deployment and publication remain separate operator actions. The shared TypeScript SDK remains private and is bundled into the OpenClaw package.

  Use OpenClaw environment/SecretRef credentials instead of `native.tokenFile`; owned daemon startup passes the token through a private stdin pipe without writing it or inheriting Gateway secrets. Clarify daemon-host profile paths and the unverified status of macOS/Windows profile compatibility in the published migration guide.

  Include the versioned Changesets package changelog entries in GitHub release notes, preserving Markdown and operator notes during release retries. Fail release preparation when exact-version changelogs are missing or entirely empty.

  Prepare the native rewrite for `1.0.0-beta.0` through Changesets prerelease mode and a CI-generated version PR. Keep beta npm/GitHub channels separate from stable, retain exact-version image/artifact identities and reject mismatched prerelease publication.

## 0.3.1

### Patch Changes

- [#195](https://github.com/thisnick/agent-wechat/pull/195) [`a71cf8b`](https://github.com/thisnick/agent-wechat/commit/a71cf8b66f99978b6c53bb43a7398f42692a9284) Thanks [@thisnick](https://github.com/thisnick)! - Accept valid phone images with recoverable metadata warnings and use best-available image retrieval by default in the HTTP API, CLI, and OpenClaw. The reported quality distinguishes original, standard, and thumbnail copies; callers can request strict full resolution with `quality=full` or `wx messages media --full`.

## 0.3.0

### Minor Changes

- [#193](https://github.com/thisnick/agent-wechat/pull/193) [`b288698`](https://github.com/thisnick/agent-wechat/commit/b2886984f0576e84d014a094620416165b91d852) Thanks [@thisnick](https://github.com/thisnick)! - Add queued voice-note sending with 50-second audio splitting, job status and cancellation, CLI support, and OpenClaw voice reply delivery.

## 0.2.0

### Minor Changes

- [#187](https://github.com/thisnick/agent-wechat/pull/187) [`93cdb04`](https://github.com/thisnick/agent-wechat/commit/93cdb0482629b90f48ade6aa7dc111586f8117c8) Thanks [@thisnick](https://github.com/thisnick)! - Return the requested image quality without silently substituting a thumbnail. The CLI requests full resolution by default; use --thumbnail for the cached preview. HTTP clients that omit quality retain the legacy thumbnail-first behavior.

  Validate cached file attachments against their message's size and content hash before returning them, and reject titles containing filesystem paths.
