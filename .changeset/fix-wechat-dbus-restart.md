---
"@agent-wechat/agent-server": patch
---

Fix WeChat restart kill loop caused by wrong DBUS_SESSION_BUS_ADDRESS

The health monitor's spawn_wechat was passing the DB-stored D-Bus address
when restarting WeChat, which could differ from the D-Bus session that
AT-SPI is connected to. This caused restarted WeChat instances to have an
empty a11y tree, triggering repeated unresponsive detection and kill cycles.
Now inherits the correct D-Bus address from the agent-server process environment.
