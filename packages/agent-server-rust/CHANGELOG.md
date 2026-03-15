# @agent-wechat/agent-server

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
