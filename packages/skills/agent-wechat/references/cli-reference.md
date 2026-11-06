# `wx` CLI 完整命令与参数参考手册

本文档汇总了 `agent-wechat` 原生 CLI 工具 `wx` 的全部命令行参数、子命令结构、默认值及约束说明。

---

## 一、 全局参数 (Global Options)

所有全局参数可置于命令的最前部或子命令后生效：

| 参数 | 缩写 | 类型 | 默认值 | 作用说明 |
| :--- | :---: | :---: | :---: | :--- |
| `--json` | 无 | Flag | `false` | 开启机器可读的 JSON 结构化输出（自动化脚本必选） |
| `--account <ID/ALIAS>` | 无 | String | 默认账号 | 指定当前操作针对的目标微信账号 ID 或别名 |
| `--config <PATH>` | 无 | Path | 自动检测 | 客户端配置文件路径（覆盖 `~/.config/agent-wechat/client.toml`） |
| `--server <URL>` | `--url` | String | 本地服务端 | 指定连接的服务端 HTTP API 地址（如 `http://127.0.0.1:6174`） |
| `--server-name <NAME>` | 无 | String | 无 | 使用客户端配置中保存的指定服务器名称（如 `local`） |
| `--server-config <PATH>`| 无 | Path | 自动检测 | 服务端配置文件路径（针对 `serve` 或 `daemon start`） |
| `--data-dir <PATH>` | 无 | Path | 自动检测 | 覆盖持久化数据根目录（默认 `~/.local/share/agent-wechat`） |
| `--token-file <PATH>` | 无 | Path | 自动检测 | 覆盖读取 API Token 的文件路径 |
| `--token-stdin` | 无 | Flag | `false` | 从标准输入管道读取 API Token（防进程列表参数泄露） |
| `--listen <ADDR>` | 无 | SocketAddr | `127.0.0.1:6174` | 覆盖服务端本地监听绑定的地址和端口 |
| `--network-listen <ADDR>`| 无 | SocketAddr| 无 | 额外的 HTTPS 监听地址（远程机器连接用，需提供 TLS 文件） |
| `--tls-cert-file <PATH>`| 无 | Path | 无 | TLS 证书公钥文件路径 |
| `--tls-key-file <PATH>` | 无 | Path | 无 | TLS 私钥文件路径 |
| `--ca-cert-file <PATH>` | 无 | Path | 无 | 客户端连接远程服务端时信任的额外 CA 根证书 |

---

## 二、 顶级命令树与子命令参数

### 1. 基础信息命令
* `wx version [--json]`：输出当前 CLI、原生 Rust 引擎版本及支持的目标架构。
* `wx status [--json]`：显示当前客户端连接状态、守护进程连接性及账号就绪度。
* `wx licenses`：查看项目及所依赖开源组件的许可证信息。

### 2. `wx daemon` - 守护进程后台管理
* `wx daemon start`：后台拉起守护进程，并记录受保护的 PID 进程文件；
* `wx daemon status`：检查后台守护进程是否正在运行、当前 PID、运行时间及端口；
* `wx daemon logs [--follow]`：读取守护进程日志，`--follow` 实时滚动；
* `wx daemon stop`：优雅关闭后台守护进程，释放文件锁与端口。

### 3. `wx serve` - 服务端前台运行
* `wx serve`：在前台启动守护进程，日志输出至标准输出；
* 适用场景：本地调试、Docker 容器内主进程或通过 systemd 前台托管。

### 4. `wx config` - 配置管理
* `wx config show`：解析并展示当前生效的配置文件位置、持久化目录及服务器列表；
* `wx config client`：查看或初始化客户端配置文件；
* `wx config server add <NAME> --url <URL> --token-file <PATH>`：添加可连接的远程/本地服务端；
* `wx config server list`：列出所有已配置的服务器；
* `wx config server use <NAME>`：切换默认使用的服务器。

### 5. `wx accounts` / `wx account` - 账号与能力
* `wx accounts list`：列出当前守护进程管理的所有微信账号槽位与登录态；
* `wx accounts create [ID] [--alias <ALIAS>]`：新建一个空白账号槽位；
* `wx accounts get [ID]`：获取指定账号的详细信息；
* `wx accounts default <ID>`：设置全局默认操作的账号；
* `wx accounts alias <ID> <ALIAS>`：为指定账号设置易读的别名（如 `work`、`personal`）；
* `wx accounts remove <ID> --confirm <ID>`：从管理器中移除已登出的账号（保留本地数据库）；
* `wx account status`：查看当前账号的在线/会话状态；
* `wx account settings [--auto-download-max-mb <N>]`：查看或设置大文件/多媒体自动下载大小门限（默认 50MB）；
* `wx account capabilities`：展示服务端和客户端当前支持的原生协议功能清单；
* `wx account connect [--detach] [--wait <SECS>]`：主动发起网络连接；
* `wx account disconnect`：断开网络连接（不清除登录凭据）。

### 6. `wx auth` - 认证与登录管理
* `wx auth login`：
  * `--official[=PATH]`：在位接管官方客户端 Profile（Linux 环境最推荐，免扫码）；
  * `--strategy <auto|saved-device|qr>`：登录策略（默认 `auto` 优先尝试已保存信任）；
  * `--force-qr`（或 `--qr`）：强制弹出二维码登录；
  * `--no-qr`：仅使用已保存设备凭证登录，失败时不降级展示二维码；
  * `--qr-output <PATH>`：将登录二维码 PNG 图片保存至指定路径；
  * `--qr-refresh <auto|manual>`：二维码过期时是否自动刷新（默认 `auto`）；
  * `--max-refreshes <N>`：二维码最大自动刷新次数（0~3，默认 3 次）；
  * `--timeout <SECS>`：整体等待登录完成的超时秒数（默认 300 秒）；
  * `--detach`：提交登录任务后立即返回操作 ID，不阻塞等待扫码。
* `wx auth operation <OPERATION_ID>`：查询正在进行的登录操作状态；
* `wx auth qr <OPERATION_ID> [--output <PATH>]`：拉取指定操作当前的二维码图片；
* `wx auth cancel <OPERATION_ID>`：取消正在进行的登录尝试；
* `wx auth logout [--detach] [--wait <SECS>]`：执行原生微信安全登出；
* `wx auth forget --yes`：删除本地登录认证，保留历史记录。

### 7. `wx messages` - 消息收发与处理
* `wx messages send <THREAD>`：向指定私聊微信号或群聊 ID 发送消息。
  * `--text <TEXT>`：发送纯文本内容；
  * `--image <PATH>`：发送本地图片；
  * `--file <PATH>`：发送普通文件或文档；
  * `--video <PATH> [--video-quality <normal|original>]`：发送视频；
  * `--voice <PATH>`：发送 SILK 语音留言；
  * `--share <URL> --title <TITLE> [--description <DESC>] [--thumbnail-url <URL>]`：发送外链分享卡片；
  * `--quote-message-id <MSG_ID> --quote-server-id <SERVER_ID> --text <TEXT>`：引用某条消息回复；
  * `--mention <ACCOUNT_ID>... --text <TEXT>`：在群聊中 `@` 提醒指定群成员；
  * `--mention-all --text <TEXT>`：在群聊中 `@所有人`；
  * `--idempotency-key <KEY>`：客户端指定的唯一幂等键，防止重复发送；
  * `--detach`：提交发送任务后立即返回，不等待服务端确认。
* `wx messages list <THREAD> [--limit <N>] [--cursor <CURSOR>]`：分页拉取聊天历史消息；
* `wx messages get <THREAD> <MSG_ID>`：获取指定消息的明细；
* `wx messages recall <MSG_ID> --server-id <EXACT_SERVER_ID>`：撤回自己 2 分钟内发出的消息；
* `wx messages media <THREAD> <MSG_ID> [--quality <best|thumbnail|standard|original>] [--output <PATH>] [--cached-only]`：下载消息中的图片、视频、语音或附件；
* `wx messages read <THREAD> --expected-version <VERSION>`：上报会话已读；
* `wx messages send-status <SEND_ID>`：查询异步发送任务的状态；
* `wx messages sync`：主动触发一次全量增量同步。

### 8. `wx chats` / `wx threads` - 聊天与会话
* `wx chats list [--limit <N>] [--cursor <CURSOR>] [--kind <direct|group>]`：列出聊天会话；
* `wx chats get <ID>`：获取单个会话的信息（未读数、最后消息、置顶标记等）；
* `wx chats find <QUERY>`：在本地搜索聊天会话；
* `wx chats pin <ID>` / `wx chats unpin <ID>`：置顶 / 取消置顶指定聊天；
* `wx chats mute <ID>` / `wx chats unmute <ID>`：开启 / 关闭会话免打扰；
* `wx chats read <ID> --expected-version <VERSION>`：标记会话为已读；
* `wx threads [--limit <N>] [--cursor <CURSOR>]`：查看会话线程列表概览。

### 9. `wx groups` - 群组与成员管理
* `wx groups list [--limit <N>] [--cursor <CURSOR>]`：列出所有加入的群聊；
* `wx groups get <ROOM_ID>`：获取指定群聊的详细信息；
* `wx groups create <MEMBER_ID_1> <MEMBER_ID_2>... [--name <NAME>]`：创建新群聊（初始成员 2~40 人）；
* `wx groups rename <ROOM_ID> <NEW_NAME>`：修改群聊名称；
* `wx groups my-name <ROOM_ID> <DISPLAY_NAME>`：修改自己在该群内的群昵称名片；
* `wx groups clear-announcement <ROOM_ID>`：清除群公告内容；
* `wx groups leave <ROOM_ID>`：退出群聊；
* `wx groups lookup-group <ROOM_ID>`：向云端精确拉取群资料；
* **群成员管理子命令**：
  * `wx groups members list <ROOM_ID> [--limit <N>]`：列出群成员名单；
  * `wx groups members get <ROOM_ID> <USERNAME>`：获取单个成员在群内的详细资料；
  * `wx groups members add <ROOM_ID> <USER_1> <USER_2>...`：邀请成员入群；
  * `wx groups members remove <ROOM_ID> <USER_1>...`：将成员移出群聊；
  * `wx groups members refresh <ROOM_ID>`：强制从云端刷新群成员最新名单。

### 10. `wx contacts` - 通讯录与联系人
* `wx contacts list [--limit <N>] [--cursor <CURSOR>]`：分页列出联系人；
* `wx contacts search <QUERY>`：搜索本地已保存的联系人；
* `wx contacts lookup-contacts <USER_ID>...`：向微信服务器精确查询用户的最新在线资料；
* `wx contacts avatar <USER_ID> [--output <PATH>] [--refresh]`：获取并导出联系人头像；
* `wx contacts sync`：增量同步联系人变更。

### 11. `wx events` - 事件总线与监听
* `wx events watch [--after <CURSOR>] [--cursor-file <PATH>]`：持续挂起监听实时消息与变更事件；
* `wx events list [--after <CURSOR>] [--limit <N>]`：拉取指定游标后的历史事件批次；
* `wx events ack --consumer <NAME> --through <CURSOR>`：向守护进程确认指定消费者已处理的游标。

### 12. `wx tokens` - 访问令牌权限管理
* `wx tokens list`：列出当前配置的所有 API 访问令牌；
* `wx tokens create <NAME> --accounts <ACCOUNTS...> --scopes <SCOPES...> [--expires-at <TIMESTAMP>]`：创建受限令牌；
  * 权限范围（Scopes）：`read`, `media`, `send`, `read-state`, `login`, `admin`。
* `wx tokens revoke <TOKEN_ID>`：吊销指定的 API 访问令牌。

### 13. `wx store` - 离线存储与底层操作
*要求：守护进程与官方微信客户端必须处于关闭状态。*
* `wx store list`：列出本地注册的数据存储目录；
* `wx store attach --path <PATH> --source-stopped`：原地挂载离线官方 Profile；
* `wx store import --source <SRC> --source-stopped`：导入 Profile 副本；
* `wx store export --destination <DEST> --source-stopped [--allow-incomplete]`：导出 Profile 副本；
* `wx store schema`：检查并清点当前数据库架构指纹；
* `wx jobs repair-writeback [--limit <N>]`：对账并修复缓存中的多媒体文件到文件系统。
