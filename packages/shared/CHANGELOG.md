# @agent-wechat/shared

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
