# 独立原生客户端 (Standalone native client)

[English](README.md) | [简体中文](README.zh-CN.md)

本 Rust 工作区实现了原生的 `wx` 可执行文件、可复用的协议/Profile 库以及经过身份验证的多账号守护进程。它既不加载也不启动官方微信可执行文件。Docker 仅作为可选的编译器或部署包装层。

受支持的开发和发布基准为 Linux x64 和 ARM64、glibc 2.36+、Rust 1.93.1。兼容性已针对特定官方 Linux Profile 家族进行了调研；未知的官方数据布局需要单独验证。CI 会在两种 CPU 架构上运行模拟协议、Profile 和运行时检查；受控的真实账号/Profile 验证是在 x64 上完成的。

初始支持的范围是原地接管现存已停止运行的官方 Profile 并继续进行受支持的写入。从零创建 Profile、导出 Profile 以及将其归还给官方客户端属于实验性拓展，不属于当前范围的强制要求。未知文件予以保留；不维护官方的 FTS 全文搜索索引。

## 已实现的能力范围

| 领域 | 已实现的行为 | 验证情况 / 剩余限制 |
| --- | --- | --- |
| 原生认证 | 固定证书的 MMTLS/混合传输、主动验证/续约、已保存设备及条件性白色二维码工作流 | 受控原生交互已通过；信任有效期和任意账号兼容性仍需单独检查。传输/完整性失败后不回退到二维码。 |
| 传入消息同步 | 独立的序列化账号 Worker、心跳轮询、受限重连、原始命令暂存及独立的投影回执 | 轮询上限为 30 秒；未主动请求的账号关联推送仍未验证。Sync 并非服务端全量历史。 |
| 本地存储 | SQLCipher 快照、带账号限定的 ID、联系人/会话/群组/成员名单、媒体读取器及数据库架构指纹 | 已验证已停止的加密快照；未知的官方架构将明确报错，而非自动迁移。 |
| 官方 Profile 接管 | 单一开关切换、已保存的身份/密钥信息/路由、通过验证密码准入所有加密数据库 | 原始 Profile 历史记录与冷重启已通过；随后全新 Sidecar 从官方保存的文件中登录成功，无需二维码或进一步手机确认。内置受支持构建的 MMKV 格式常量。 |
| Profile 回写 | 经架构校验的文本、元数据、受支持媒体及桌面端已读写入器，具备跨文件可恢复回执 | 已校验受支持资源关联的传出历史记录行和图片/语音/文件/视频缓存，包括本地修复。不支持的命令保持暂存。FTS 搜索索引和任意官方架构不在受支持写入器范围内。 |
| 文本/多媒体发送 | 持久化幂等文本意图及图片/文件/视频/语音任务、单次原生提交、部分/不确定回执 | 受控的文件传输助手接受了现有多媒体路径，经所有者确认文件/图片可正常打开，视频/语音可正常播放。常规文件具有 512 MiB 代码上限；32/50 MiB PDF 通过了原生验收、接收方打开及冷启动字节/数据库校验，而 512 MiB 最终客户端回读仍未完成。视频源允许 50 MiB（支持标准/原始预处理）；标准和原始控制项通过了服务端/回读/冷 Profile 校验并在手机端得到确认。图片/语音上限保持在 26,214,383 字节。大文件缓存命中暂不支持。语音使用内置 SILK 独立编码；FFmpeg/FFprobe 标准化媒体源。表情发送已禁用。 |
| 多媒体读取 | 区分声明与实际变体、校验缓存、严格画质请求、持久化传输任务及 Range 分片读取 | 下载了三种 JPEG 画质及标准/原始 H.264/HEVC 视频，并进行了哈希/分片或解码器检查。挂起、过期、不支持和可用状态保持区分。缓存字节在远端过期后依然有效；原始 XML 绝不作为附件返回。 |
| 元数据刷新 | 联系人/群组查找、独立目录/检查点、群成员名单及头像缓存 | 无会话的群聊、7 人成员名单和小头像通过了原生拉取。原生联系人列表在检查点间恢复并同步完毕，其中 3 个明确不可用的联系人作为部分水合单独上报。 |
| 客户端与接口 | 限定账号范围的 API、经认证的资源游标、重放/SSE、生成的 Rust→TS 契约、原生 CLI/SDK | CLI 生命周期和传输固件在无需账号凭证的情况下测试真实二进制文件。逻辑删除墓碑保留已删除视图的位置。 |
| OpenClaw 集成 | 托管/外部原生宿主、持久化状态留存、准入文本/媒体、原生发送及可选 Gateway 外观 | 实际固定版本的 Gateway/本地模型测试验证了单聊和群聊轮次中的关联文本、图片字节和文件/视频/语音引用。需要显式的主机权限；遗漏、不支持或有歧义的输入保持未读。 |

[FEATURES.md](FEATURES.md) 详细区分了必需的实现、针对性测试以及剩余的真实环境验证。[CLI.md](CLI.md) 涵盖了配置、后台所有权、登录、离线 Profile、媒体、回执及退出码。未知的命令保持在 Sidecar 中加密，直到受支持的官方投影成功。接受的同步检查点并不代表每个官方文件都已追赶完成。

## 构建与验证

在仓库根目录下执行：

```bash
cargo build --manifest-path packages/native-client-rust/Cargo.toml --workspace --locked
cargo test --manifest-path packages/native-client-rust/Cargo.toml --workspace --locked
cargo fmt --manifest-path packages/native-client-rust/Cargo.toml --all --check
cargo clippy --manifest-path packages/native-client-rust/Cargo.toml --workspace --all-targets --locked -- -D warnings
python3 packages/native-client-rust/scripts/smoke.py packages/native-client-rust/target/debug/wx
node scripts/native-openclaw-smoke.mjs /absolute/path/to/wx
python3 scripts/native-cli-smoke.py /absolute/path/to/wx
python3 scripts/native-cli-fixture-test.py /absolute/path/to/wx
python3 scripts/native-export-smoke.py /absolute/path/to/wx
bash scripts/native-generate-types.sh --check
# 在运行以下固定宿主测试前，需先构建扩展及其共享依赖：
node scripts/native-gateway-smoke.mjs
node scripts/native-model-smoke.mjs
node scripts/native-media-model-smoke.mjs
```

公共冒烟测试使用合成数据、环回 HTTP 及独立的子进程，不发起微信请求；模型边界测试使用本地桩模型提供者和确定的本地语音转录固件。单元测试涵盖认证加密向量、畸形/跨账号数据、无符号 64 位 ID、真实 SQLCipher 数据库、架构漂移、复制保留、投影重试、整会话已读、Token 范围/吊销、事件状态留存及不确定发送恢复。原生打包和部署准备请参见 [deploy/native](../../deploy/native/README.md)。测试命令绝不会部署生产服务。

私有测试账号验证额外演练了原生过期/续约、心跳/同步、组装后守护进程的复制官方历史，以及已停止运行的加密 Profile 克隆。保留了原始来源/检查点哈希。私有凭证、Profile、抓包数据和消息均排除在本仓库之外。这些检查并不代表与任意账号或官方构建的通用兼容性。

## 服务端与客户端配置

默认服务端配置文件：`$XDG_CONFIG_HOME/agent-wechat/server/config.toml`，回退至 `~/.config/agent-wechat/server/config.toml`：

```toml
schemaVersion = 1
listen = "127.0.0.1:6174"
dataDir = "/srv/agent-wechat"
# 可选的预配所有者专用引用：
# tokenFile = "/run/secrets/wechat-api-token"
# storageKeyFile = "/run/secrets/wechat-storage-key"
# profileFormatKeyFile = "/run/secrets/wechat-profile-format-key"
```

当该文件存在时，除 `schemaVersion` 外所有字段均为可选。`profileFormatKeyFile` 覆盖受支持构建内置的 MMKV 格式常量；它与账号授权、数据库加密和守护进程存储密钥彼此独立。缺失的默认文件使用默认值；显式选择但缺失的文件、未知键和不支持的版本将报错。相对文件引用基于所选配置目录解析。`--server-config` 覆盖 `AGENT_WECHAT_SERVER_CONFIG`；`--data-dir` 覆盖 `AGENT_WECHAT_DATA_DIR`、文件设置以及 XDG 默认值。`--listen` 和 `--token-file` 覆盖文件中的值。该服务端仅绑定环回地址；远程客户端请使用经过身份验证的 TLS 反向代理或受保护的加密隧道。

持久化根目录默认为 `$XDG_DATA_HOME/agent-wechat`，回退至 `~/.local/share/agent-wechat`：

```text
registry.sqlite
api-token
storage-key
accounts/<local-id>/profile/       # 实验性托管/复制 Profile
accounts/<local-id>/agent/control.sqlite
```

直接绑定的官方 Profile 保留在其选定的外部位置，不会隐式导入。凭证、原始批处理和已排队的发送载荷均使用带有独立存储密钥的认证加密进行保护。API Token 不是数据库或存储密钥。缺失或错误的存储密钥会导致失败，而不会覆盖受保护的状态。请将持久化根目录和所有外部机密引用一同备份。仅导出 Profile 不包含守护进程 Sidecar 及其活跃授权协调器。写入所选 Profile 中的官方登录数据会随该 Profile 一并复制，因此导出的目录树仍属于私有账号数据。

`wx config show` 仅解析设置而不初始化存储。初次运行 `wx serve` 会隐式预配私有的 Token/密钥文件；显式引用必须已存在并满足私有文件检查。所有者 API Token 在启动时加载。`serve` 在前台运行直到按下 Ctrl-C 或收到 SIGTERM。`wx daemon start|status|stop|logs`、提供的 systemd 服务单元或 OpenClaw 可以管理其后台生命周期；请选择单一管理者。停止服务会刷盘持久化工作并释放所有权，而不会执行原生登出。

客户端文件：`~/.config/agent-wechat/client.toml`（支持 XDG 规范）：

```toml
schemaVersion = 1
activeServer = "local"

[servers.local]
url = "http://127.0.0.1:6174"
tokenFile = "/srv/agent-wechat/api-token"
defaultAccountId = "example"
```

可使用 `--config`、`--server-name`、`--server URL`、`--token-file` 或 `--account` 覆盖选项。旧版单一端点客户端文件仍可读取。保存的默认设置属于其对应的端点。多个账号未指定默认值将在执行任何操作前报错。API Bearer 字节可来自 `AGENT_WECHAT_TOKEN`；显式 `--token-file` 优先级更高。Token 切勿放在 URL 或命令参数中。HTTP 仅限环回地址，拒绝重定向。离线 `profile` 命令不解析 Token 也不连接守护进程。

## 选择已初始化的官方 Profile

在守护进程运行期间，关闭官方客户端并使用单一开关。绝不要将它们作为并发写入者运行：

```bash
wx auth login --official
# 或在守护进程主机上指定显式路径（HOME、xwechat_files 或活跃账号根目录）：
wx auth login --official=/absolute/path/to/profile
```

该命令将发现或选择操作文件、注册账号、绑定其身份、尝试可用的已保存设备信任并条件性地展示二维码。在通过账号/设备/密码验证后，它会自动派生本地密钥并启用受支持的回写。`--official` 将数据保留在原地；官方客户端必须保持关闭。发现机制在检测到多个 Profile 时会拒绝执行。持久化的 `official` 存储模式将导出范围限定为 `xwechat_files` 和 `.xwechat`，排除无关的 HOME 文件和原生 Sidecar。其他 Profile 绝不会被静默替换。详情参见[登录工作流](CLI.md#login-and-logout)。

`.xwechat/net/host` 下现有的安装密钥信息和已保存的区域主机重定向将被恢复，用于已保存设备确认和二维码恢复。已存储的完整设备 ID 保持不变；原生请求使用其前 16 字节前缀。扫码确认受登录超时时间约束，而非二维码图片的显示过期时间。登录失败会严格区分密钥验证、数据库打开、文件系统及快照错误。这些保护措施并不保证能成功接管任意官方 Profile。

## 实验性 Profile 复制与初始化

导入/导出以及全新创建功能可用于实验；它们不属于初始接管范围。导出是对报告了投影差距的数据的字节级复制，不是架构转换器，也不证明官方客户端的兼容性。

对于更底层的离线注册，请先停止两端的写入：

```bash
wx --data-dir /private/native-data store attach --account test \
  --path /private/stopped-official-home --source-stopped
wx --data-dir /private/native-data store list
```

底层直连模式可以选择包含 `xwechat_files/<account>/db_storage` 的完整官方 HOME 目录，也可以选择包含 `db_storage` 的现有账号目录。托管复制/导入应保留完整的官方 HOME 目录及原始账号目录名：

```bash
wx --data-dir /private/native-data store import --account copied \
  --source /private/stopped-official-home --source-stopped
wx --data-dir /private/native-data store export --account copied \
  --destination /private/new-official-home --source-stopped
```

目标路径必须不存在且其父目录必须存在。复制保留不透明的普通文件字节、WAL/SHM、未知文件、空目录、文件权限模式、mtime 以及安全的内部相对软链接。前后对比清单会检测观察到的源文件变化，但无法证明完全不存在不受控的外部写入者。不支持的链接/特殊文件、超大复制以及重叠路径均会失败。不保留所有权/ACL/扩展属性及硬链接共享。不执行数据库转换或信任刷新。

导出操作会拒绝处于挂起状态的同步批次、游标/已保存登录发布、已读更新、元数据回执及已接受的文本/媒体投影。已保存登录回执必须与确切接受的加密授权代际相匹配。如果可用的已缓存媒体在普通文件回写中存在部分、不受支持或未知的情况，即使其历史行已完整，也需要覆盖参数；`--allow-incomplete` 显式仅复制当前的官方文件，并将每个挂起类别报告为 `incompleteCopy: true`；媒体缓存差距显示为 `incompleteMediaWriteback`。收到游标但缺失其确切封装代际的发布回执时同样需要该覆盖参数。`syncCheckpointProjected: true` 记录了匹配的原生发布回执且无挂起的同步/游标/撤回任务；它并不证明原版客户端已接受，也不代表独立元数据/已读投影的状态。缺乏该证据时该字段为 false，并通过 `syncCheckpointEvidence` 区分挂起与未验证。复制不会恢复排队的任务、消费者回执或活跃协调器。使用 `jobs repair-writeback --limit 25` 可在无需网络传输或重发的情况下将已接受/缓存的媒体对账到受支持的普通文件中；在导出前请先检查受阻结果。请保留整个原生持久化根目录以供恢复。导入发布与注册表提交之间的故障可能会留下未注册的目录；请将其保留以供检查，不要将其删除或覆盖重试。

Profile 租约排除了跨数据根目录的协同原生操作。未受管的官方应用不遵循这些租约。`--source-stopped` 是操作员的确认断言，而非对官方进程的自动检测。

绑定授权的私有密钥文件，或从显式提供的私有 32 字节账号密码文件中派生每文件密钥：

```bash
wx --account test store bind-keys --keys-file /private/profile-keys.json
wx --account test store derive-keys --profile /private/account-directory \
  --password-file /private/account-password.bin --source-stopped
wx --account test store schema
```

派生的密钥保存在账号的守护进程 Sidecar 下，位于所选官方目录树之外。CLI 不会从运行中的进程获取账号密码或活跃凭据。密钥文件格式、旧版 Agent 密钥缓存导出库 API 以及读取器限制参见 [wechat-profile](crates/wechat-profile/README.md)。数据库密钥可用性和图片密钥资料并不能证明每个数据页/图片都能成功解码。

在调研的家族中，官方架构没有公认的全局单调版本标识。SQLite 的 `schema_version` 并非应用程序兼容性版本。读取器清点架构和指纹；写入器验证其受支持的确切结构，并拒绝未知的字段/索引/触发器/路由，而不是擅自迁移官方存储。Sidecar 迁移拥有独立的版本。

如需尝试仅原生全新创建，可使用 `wx accounts create --alias test` 随后执行 `wx --account test auth login`。接受登录资料后会初始化 7 个受支持的加密数据库和已保存登录文件。完整的官方首次运行初始化、搜索索引及官方 UI 渲染仍未经验证。请妥善保留现有的 Profile 及其备份。

## 登录、同步与读取

```bash
wx serve
# 在另一个终端中：
wx auth login --official=/absolute/stopped-official-home
wx accounts list
wx accounts alias ACCOUNT_ID test
wx --account test threads
wx --account test contacts --search Example
wx --account test groups
wx --account test lookup-contacts username
wx --account test lookup-group 'room@chatroom'
wx --account test messages list filehelper --limit 50
wx --account test messages get msg_OPAQUE_ID
wx --account test events --after 0 --watch
```

登录是一个异步操作。`--detach` 会返回其操作 ID；使用 `auth operation ID`、`auth qr ID --output PATH` 和 `auth cancel ID` 可进行查看与控制。二维码输出为带有白边静区的白底不透明 PNG。新的代际使用独立的输出文件。扫码/等待确认期间挑战保持稳定，仅在扫码前且在受限的总体截止时间内刷新，确认后立即完成最终认证交换。

已保存登录的回写会将接受的区域业务主机包含在官方路由 XML 及选择器中。因此通过 `--official` 重新打开的 Profile 会保留其路由。可恢复的凭据日志同样覆盖这些文件；中断的发布保持挂起，直到路由和凭据均通过回读。现有无关的映射与设置均予以保留，发生改变的源文件将被拒绝。

接受活跃心跳可恢复已保存的会话；经过认证的过期允许续约一次。已保存设备确认与显式二维码是不同路径。传输/完整性失败不会清除凭据，也不会断定信任过期。自动重连绝不打开二维码。离线恢复的已保存凭据在当前进程完成被接受的原生交互前仍属未经验证。

`auth disconnect` 在保留信任的同时停止期望的网络活动。`auth logout` 在可选的发起单次经认证的原生登出之前，先提交持久化的本地抑制。它将分别上报本地提交、凭证保留、远程确认/拒绝/未知结果以及未经验证的已保存设备吊销。`auth forget --yes` 是另一个经过确认的本地操作，会保留历史记录。旧版的 `auth logout-local` 仅执行本地抑制。原生活跃机密和游标保存在加密的 Sidecar 中；原生会话持久化并不保证官方客户端可以复用它们。

仅用于操作员选定的独立凭证备份/检查点：离线命令 `store restore-credentials --file /private/state.json [--cursor-file /private/raw-cursor.bin]` 会安装已保存但未经验证的状态。这不是官方 Profile 凭证提取器。原始游标是不透明字节，不是消息 ID 或 JSON；请勿修改其内容。

`messages sync` 请求一个原生页面。原始命令字节与收到的游标在投影前一同提交至原生 Sidecar。收到检查点意味着原始命令是可恢复的；这并不保证官方文件已经完成追赶。消息/会话/MMKV 数据库无法共享单个事务。确切的应用回执与幂等对账连接了受支持的写入；未知的命令保持挂起。离线命令 `store recover-projection` 或经认证的 `POST /v1/accounts/{account}/projection/recover` 可重试保留的命令而无需重新发送。

仅在确认实际的远程身份和对 Profile 的独占所有权后，才启用受限的写入器：

```bash
# 守护进程与官方客户端均已停止：
wx --account test store enable-writes --own-username OWN_USERNAME \
  --mmkv-store /private/official-home/xwechat_files/all_users/config/global_config \
  --format-key-file /private/mmkv-format-key.bin --official-client-stopped
```

显式的离线绑定命令需要提供私有的 16 字节 MMKV 格式密钥引用以及所选 Profile 内部的身份存储。受限的只读 v4 解码器会验证分帧/CRC，随后将保存的用户名和 UIN 与原生账号状态进行匹配，并检查所选账号目录的绑定。这些检查在写入前会再次执行。内置的 Profile 格式常量不是账号凭证或安全边界；账号资料和文件权限仍分别受保护。不支持的布局将拒绝写入。身份绑定不同于写入授权。接受的已保存设备/二维码凭据以及受支持的存储投影拥有经过校验的独立 MMKV/SyncKey 路径；不暗示通用的信任提取和任意版本的迁移。启用该功能不代表免除架构/索引/映射检查。仅在受支持的官方投影完成后才会发出实时传入事件；启动/追赶阶段的观察数据会单独标记，不会成为新的 OpenClaw 轮次。

## 发送、读取与事件状态留存

```bash
wx --account test messages send filehelper --text "清晰标记的测试文本" \
  --idempotency-key unique-logical-send-id
wx --account test messages send-status SEND_ID
wx --account test messages recall msg_OPAQUE_ID --server-id EXACT_SERVER_ID
wx --account test operations get OPERATION_ID
wx --account test messages read THREAD_ID --expected-version OBSERVED_READ_STATE_VERSION
```

幂等键仅能在相同目标和相同载荷时复用。排队中的发送在提交前先被持久化。被接受的结果要求匹配的目标/客户端身份和完整的服务器 ID。提交期间重启将变为 `needs_review`；对于不确定的结果绝不自动重发。相同的逻辑键返回相同的意图，若载荷改变则返回冲突。受支持的已接受发送行和媒体缓存使用持久化投影回执；不支持的绑定保持显式差距。

撤回操作需要历史记录或经校验的发送回执中的精确不透明消息 ID 及十进制字符串服务器 ID。守护进程准入单次针对自己消息的尝试，返回持久化的 `message_recall` 操作并交由微信决定是否允许撤回。CLI 在提交前先持久化目标 ID 和密钥。不确定的结果以退出码 8 结束；其他密钥无法绕过其目标声明。传入的撤回在官方投影前先提交账号/会话/服务器标记，保留 ID/顺序并发出一次 `message.recalled` 事件。已撤回的消息/引用/媒体读取操作会抑制原始内容，而加密日志、保留的文件和备份仍可用于显式恢复。挂起或不确定的撤回任务会阻塞完整的 Profile 检查点/导出。详见 [API 与存储契约](https://thisnick.github.io/agent-wechat/how-it-works/message-types/#recalls)。

已读状态变更会通过其最新观察到的消息将整个会话标记为已读，并发送一次原生跨设备通知。这可能会清除模型未消费的该会话更早的未读消息。CAS（比较并交换）版本包含观察到的 Profile 代际，因此中间发生的来源变化会导致冲突。事件投递确认（ACK）绝不会修改已读状态。不确定的原生通知保持记录状态，不自动重新提交。预处理的模型交接需要强相关的回调；接受/排队的工具执行不能作为该模型交接的证据。

该 API 要求 Bearer 请求头。核心资源位于 `/v1/accounts/{account}` 下：`threads`、`contacts`、`groups`、`groups/{room}/members`、`threads/{thread}/messages`、`messages/{id}`、缓存的 `avatars/{id}` 以及 `messages/{id}/image?quality=best|original|standard|thumbnail`。原生查找观察使用 `contacts/lookup` 和 `groups/{room}/lookup`。`auth/login`、操作/二维码/取消路由、`sends`、`threads/{thread}/read-state`、`events`、`events/stream` 和 `consumers/{consumer}/ack` 为独立操作。游标/ID 字段为字符串；消息 XML、CDN 密钥和原始数据库字段不会作为公共消息体返回。

使用 `tokens create NAME --accounts ACCOUNT --scopes read` 可签发受限 Token，并使用所有者 Token 通过 `tokens list|revoke ID` 进行管理。可授予的权限包括 `read`、`media`、`send`、`read-state`、`login`、`admin`。资源鉴权在 Profile/网络操作前进行；在 SSE 轮询期间会再次检查 Token。事件日志保留受限窗口、明确报告差距，并支持在持久化十进制游标后进行重放。事件状态留存与消息已读投影彼此独立。

## 库职责边界

- `wechat-config`：带版本控制的服务端/客户端路径与设置。
- `wechat-store`：私有根目录/注册表、Profile 复制/租约、加密记录保管库、发件箱、事件及应用意图。
- `wechat-profile`：官方 SQLCipher/架构/消息/媒体读取器及受限的校验写入器。
- `wechat-protocol`：原生密码学、传输、认证/同步/联系人/媒体报文编解码器。
- `wechat-core`：序列化账号 Worker 及持久化操作/投影协同。
- `wechat-api`：限定账号范围的 HTTP 及 SSE 路由。
- `wx-native`：CLI 与服务端的装配入口。

受支持的现有 Profile 工作流具有明确的兼容性边界，尚未准备好直接替换任意生产环境的官方 Profile。此处执行的验证不包含任何生产部署、发布或生产 Profile 交接。

`wechat-protocol` 与 `wechat-profile` 是面向开发者的公共库入口点。其底层的支撑库 `wechat-media` 与 `wechat-platform` 同样配置了 crates.io 发布支持。这四个 Crate 均与原生发布版本保持一致；工作区内的其他 Crate 仍保持非公开状态。发布注册表从包含此变更的下一个版本开始。带 Tag 的 Git 依赖或本地路径依赖可继续正常使用。安装说明、调用示例与兼容性请参见 [Rust 开发库参考](https://thisnick.github.io/agent-wechat/zh-cn/reference/rust-libraries/)，发布流程请参见 `RELEASING.md`。
