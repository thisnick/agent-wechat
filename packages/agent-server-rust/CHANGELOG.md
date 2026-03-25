# @agent-wechat/agent-server

## 0.11.11

## 0.11.10

## 0.11.9

### Patch Changes

- [#134](https://github.com/thisnick/agent-wechat/pull/134) [`2ceb514`](https://github.com/thisnick/agent-wechat/commit/2ceb51456bfb0cbc6fe96cba4aa3e2c25f653373) Thanks [@thisnick](https://github.com/thisnick)! - Keep token query param in VNC URL so the page works when accessed directly via bookmark or shared link

## 0.11.8

### Patch Changes

- [#132](https://github.com/thisnick/agent-wechat/pull/132) [`771a1c1`](https://github.com/thisnick/agent-wechat/commit/771a1c1540a6d1846a440a095121be876a7c7916) Thanks [@thisnick](https://github.com/thisnick)! - Fix VNC WebSocket auth: keep token embedded in the noVNC `path` query param so it is passed to the WebSocket connection, and remove it from the visible URL for security

## 0.11.7

### Patch Changes

- [#129](https://github.com/thisnick/agent-wechat/pull/129) [`22f132d`](https://github.com/thisnick/agent-wechat/commit/22f132d362c7362151ad670557de83d3d0ce2f29) Thanks [@thisnick](https://github.com/thisnick)! - Fix VNC redirect encoding by passing token as a separate query parameter instead of embedding it in the URL path

## 0.11.6

### Patch Changes

- [#124](https://github.com/thisnick/agent-wechat/pull/124) [`e608898`](https://github.com/thisnick/agent-wechat/commit/e60889870686f25e289aa58bd38fe35e410c36ee) Thanks [@thisnick](https://github.com/thisnick)! - Fix WeChat restart kill loop caused by wrong DBUS_SESSION_BUS_ADDRESS

  The health monitor's spawn_wechat was passing the DB-stored D-Bus address
  when restarting WeChat, which could differ from the D-Bus session that
  AT-SPI is connected to. This caused restarted WeChat instances to have an
  empty a11y tree, triggering repeated unresponsive detection and kill cycles.
  Now inherits the correct D-Bus address from the agent-server process environment.
