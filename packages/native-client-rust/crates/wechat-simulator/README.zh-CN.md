# 本地上游模拟器 (Local upstream simulator)

[English](README.md) | [简体中文](README.zh-CN.md)

开发模拟器运行真实的 Rust Hub、认证 API、账号 Worker、加密 Profile、发件箱、投影日志以及持久化的事件/SSE 流。注入的上游适配器与独立的 Rust 场景服务交换本地 JSON 数据。该场景服务拥有独立的 SQLite 数据库。

该命令提供 API/SDK 开发环境。如需使用连接的浏览器预览，请使用 `pnpm client:dev`；详见[客户端预览指南](client-ui.md)。生产环境 Web 托管与 Tauri 外壳仍属于后续工作。

## 启动

使用原生 README 中说明的指定 Rust 工具链和原生编译器依赖项。无需 Docker 和生产账号凭证。

在仓库根目录下执行：

```sh
pnpm native:dev
```

无需 pnpm 或 Node 的等效命令：

```sh
cargo run --profile test --locked --manifest-path packages/native-client-rust/Cargo.toml -p wechat-simulator --bin wx-dev -- dev
```

优化后的测试 Profile 保留了调试断言并使加密 Profile 的初始化更加实用。启动器会创建私有的临时状态，启动场景服务和 Hub 并执行健康检查，随后打印它们的地址和私有 Token 文件路径。它不打印 Token 内容。这是 Hub 的 API URL；该命令暂不提供前端服务。

按下 Ctrl+C 停止拥有的子进程并清理一次性状态。使用 `--keep-state` 保留自动分配的目录，或传入 `--data-dir PATH` 复用命名的开发目录。例如：

```sh
pnpm native:dev --data-dir ./local-simulator
pnpm native:dev --data-dir ./local-simulator --reset
```

仅接受全新/空目录或由本开发运行时标记的目录。Reset 需要两项服务均已停止，且仅清理模拟器的 Hub 和上游数据库。Token 文件予以保留。受支持的场景为 `basic`；未知的场景名称将被拒绝。

## SDK 与场景控制

使用现有的 `NativeWeChatClient`，传入打印的 Hub `endpoint` 以及从 `tokenFile` 读取的令牌。创建一个虚拟账号（如 `personal`），随后以 `qr` 策略启动其常规登录操作。

二维码图片是合成生成的，不包含真实微信登录 URL。扫码/确认通过向打印的上游地址发送认证的 `POST /control` 请求进行控制。读取独立的 `controlTokenFile` 获取该请求的 Bearer Token。这些控制器不存在于生产 Hub 的路由中。

代表性的 JSON 控制载荷：

```json
{"op":"login","account":"personal","status":"scanned"}
{"op":"login","account":"personal","status":"approved"}
{"op":"inbound","account":"personal","peer":"sim_mei","body":"合成传入消息"}
{"op":"inbound","account":"personal","peer":"sim_mei","body":"合成账号发出的消息","self_sent":true}
{"op":"inbound","account":"personal","peer":"sim_mei","body":"重启后追赶消息","notify":false}
{"op":"notify","account":"personal","count":2}
{"op":"echo_send","account":"personal","client_id":123}
{"op":"status_notification","account":"personal","operation_id":2}
{"op":"status_notification","account":"personal","operation_id":11}
{"op":"send_fault","account":"personal","fault":"reject"}
{"op":"send_fault","account":"personal","fault":"lost_acknowledgment"}
{"op":"advance","seconds":121}
{"op":"revoke","account":"personal"}
{"op":"unknown_sync","account":"personal"}
{"op":"advertise_image","account":"personal","peer":"sim_mei"}
{"op":"receive_media","account":"personal","peer":"sim_mei","kind":"sticker"}
```

账号 ID 包含小写 ASCII 字母、数字、下划线或连字符，长度上限 32 字符。虚拟对等端包括 `sim_mei`、`sim_daniel` 和 `filehelper`（文件传输助手）。服务会在初次登录时为账号预置联系人及初始会话。每个账号拥有独立的会话、变更、通知位置和发送回执。`GET /state` 返回受限的场景摘要，不包含凭证。

`advance` 控制上游挑战的过期时间和合成传入时间戳。Hub 截止时间、Worker 调度和 SSE 轮询仍使用其正常时钟。登录取消和操作截止时间仍由真实的 Hub 强制执行。

文本发送使用真实的持久化发件箱。被接受的发送保存在模拟器的持久化远程消息历史中，**默认不会回显到当前 Hub 会话的同步流中**。Hub 根据发送回执进行提交。`echo_send` 显式注入异常的迟到/重复同步回显以进行去重测试；需提供发送回执中的数字 `clientId`。

丢失确认先记录上游接受随后返回模拟失败；Hub 保留 `needs_review` 并不重新提交发送。外发的同步回显本身无法解决该不确定回执。成功的发送与回显的消息通过其规范历史身份进行对账。排队的文本发送现在与准入原子化发布 `send.updated`，使得独立的客户端可以立即观察到。登出抑制和中断发送恢复同样以原子化方式发布相应的状态变化。

## 消息事件与自身发送

客户端渲染传入和传出的消息（包括追赶历史）。在收到 `message.committed` 后，使用其 `messageId` 拉取消息；该事件不包含消息正文。`sync.received` 可以跟在消息事件之后，且不是展示该消息的前置条件。

| 事件分类 | 含义 | Agent 处理资格 |
| --- | --- | --- |
| `incoming` + `live` | 来自其他账号的新消息 | 有资格处理，受限于现有的访问/提及规则 |
| `outgoing` + `live` | 通过上游同步到达的自身发送消息 | 忽略 |
| `outgoing` + `local_send` | 通过当前 Hub 接受的发送 | 忽略 |
| 任意方向 + `catchup` | 重新连接后同步的历史记录 | 忽略 |

两种外发类型在拉取的消息上均具有账号自身的 `senderId`。手机端发送给 `filehelper` 的消息是对自身的备忘笔记，不会开启 Bot 命令功能。`local_send` 标识了由 Hub 发起的发送消息，但不区分是由哪个连接的 UI 或 Agent 发起的。现有的 OpenClaw 调度门禁已经要求 `incoming` 加上 `live`。

`status_notification` 注入当前未处理的 51 类型操作 2 或 11。它们保留在暂存中并带有投影诊断信息，阻塞完整官方游标的发布。随后到达的正常消息仍可提交并变得可读。客户端应将此类诊断信息与消息流区分开；手机端针对这些控制项的已读状态同步在此处未实现。

## 隔离与当前限制

- 开发可执行文件为 `wx-dev`；常规的 `wx serve` 没有模拟器开关。原生启动拒绝带有标记的模拟器存储。
- 模拟器启动需要经标记的隔离存储及受管 Profile。拒绝挂载官方 Profile 和导入原生凭据。
- 适配器地址为数字环回套接字地址。针对这些本地模拟器请求，重定向和代理转发均已禁用。服务和 Hub 仅绑定到环回地址并使用独立的私有 Bearer Token。
- Mock 使用已解码的同步批次和现有的命令编解码器。它未实现微信网线加密，也不代表协议兼容性。
- 受支持的操作包括合成二维码/保存会话登录、心跳、同步、文本/多媒体发送、群组创建/设置/成员变更及名单、推送提示、已读通知、联系人/头像刷新及登出。传入固件类型包括图片、文件、视频、语音和动态表情。外发类型包括图片、文件、视频和语音。媒体源、预处理、任务、回读和历史仍使用生产 Hub 流水线。多媒体对象和发送接收限定在独立模拟器数据库的账号范围内。
- 不支持的网络操作会直接失败关闭，包括原生客户端构造、HTTP 链接缩略图、任意 CDN/URL 检索、分享和撤回。头像和表情检索仅接受精确的本地固件身份。表情 URL 仅作为元数据，绝不被模拟器解析或拉取。`advertise_image` 继续演练不包含拥有字节的不可用图片；`receive_media` 则提供校验过的本地字节，而不会仅仅因为收到通知就将其标记为可用。
- 构建依赖项可能需要从网络下载。正在运行的场景仅使用本地服务。回归测试流程也可以在禁用网络的容器中运行以验证运行时隔离。
- 来自另一台模拟设备的外发消息用于测试分类与投影。官方客户端真实自身发送的投递、时序和共存需要单独的证据支持。

## 验证

```sh
pnpm native:simulator:smoke
```

冒烟测试会启动真实的 `wx-dev upstream` 和 `wx-dev hub` 子进程。它检查实际 `wx` CLI 的状态与消息拉取，并使用现有的 TypeScript SDK 验证二维码扫码/确认、加密 Profile 投影、两个独立的 SSE 消费者、排队/接受/不确定的发送、幂等性与显式注入故障的同步回显去重、向文件传输助手自身发送实时消息、传入推送、游标恢复、Hub 重启与追赶、账号隔离、会话吊销/登出、虚拟时钟二维码过期、不可用声明媒体、不阻塞后续消息的状态控制诊断、保留未知同步命令以及启动器资源清理。环境变量 `WX_DEV_BINARY` 和 `WX_BINARY` 可指定已构建的开发版和 CLI 二进制文件；默认均从 `target/debug` 查找。

Rust 测试覆盖了丢失确认及服务重启后的持久化远程接受、确定性固件、时钟控制及存储/进程隔离。它们对现有的原生协议/核心/Profile/API 测试形成了补充。合成测试结果不代表官方客户端渲染或真实验收。

对于已经构建完成的 Linux 工作区，Docker 可以验证在外部网络禁用的情况下测试流程依然正常工作。在环境准备阶段拉取 Node 镜像，随后运行：

```sh
docker run --rm --network none --read-only \
  --user "$(id -u):$(id -g)" --tmpfs /tmp:rw,nosuid,nodev,mode=1777 \
  --mount "type=bind,src=$PWD,dst=/app,readonly" --workdir /app \
  node:24-trixie-slim node scripts/native-simulator-smoke.mjs
```

Linux x64 验证于 2026-10-22 通过了完整的 Rust 工作区套件、锁定的 Format/Clippy 检查、生成契约检查、TypeScript 检查、SDK 测试和 OpenClaw 测试。完整的模拟器流程在带有 `--network none` 的只读非 root 验证镜像中也全部通过；仅限连接的系统跟踪仅记录了环回地址。Linux ARM64、macOS ARM64 和 Windows x64/ARM64 运行时检查已在 CI 中配置，但尚未在此工作区中运行。

群组模拟器现已在合并的原生群组 RPC 边界处替换原始本地回复，使用检入的 Linux 编解码固件作为响应形态。真实的 Core 仍然验证普通联系人/成员身份、记录稳定密钥、封装请求/回复、投影已知结果并处理不确定输出。建群、重命名、自身群昵称、添加/移除、公告清除、免打扰开关和三步退群通过公开 SDK 进行了演练。原生大文件/视频路径保留其预留、进度、回读和授权围栏；不支持的超大模拟器路由会在进入任何原生网络传输前失败。
