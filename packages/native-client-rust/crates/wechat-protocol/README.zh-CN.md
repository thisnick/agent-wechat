# wechat-protocol

[English](README.md) | [简体中文](README.zh-CN.md)

针对 Linux 4.1.13.26 Profile 的同步阻塞式原生协议库。这是使用 OpenSSL 原语实现的纯 Rust 协议代码；它不执行 Node 也不加载官方微信可执行文件或动态库。异步调用方必须使用阻塞 Worker。

## 从 NativeClient 开始

大部分服务端操作都是 [`NativeClient`](https://docs.rs/wechat-protocol/latest/wechat_protocol/struct.NativeClient.html) 上的方法。Crate 顶层的函数列表主要包含辅助工具函数。

| 任务 | API 方法 |
| --- | --- |
| 登录认证 | `start_qr_login`、`poll_login`、`complete_login` |
| 接收与同步 | `sync`、`sync_scene`、`heartbeat`、`push_connection` |
| 发送消息 | `send_text`、`send_share`、`send_quote`、`send_file`、`send_image`、`send_video`、`send_voice` |
| 联系人与群 | `lookup_contacts`、`lookup_room`、`init_contacts` |
| 底层操作 | `wire`、`media`、群聊请求构建器与响应解码器 |

`AccountState` 保存已被服务端接受的凭据，并以可变引用形式传入，因为服务端响应可能会更新凭据或路由状态。后台守护进程的原生上游适配器调用这些 API；并在其之上提供了账号协调、故障恢复与 Profile 数据回写能力。部分底层操作使用请求构建器、客户端 RPC 方法与解码器组合完成，而非单一的快捷方法。

在本地代码仓库中，可以直接运行以下示例：

```bash
cargo run --manifest-path packages/native-client-rust/Cargo.toml -p wechat-protocol --example qr_login
cargo run --manifest-path packages/native-client-rust/Cargo.toml -p wechat-protocol --example send_text -- RECIPIENT UNIQUE_TASK_ID "来自 Rust 示例的问候"
cargo run --manifest-path packages/native-client-rust/Cargo.toml -p wechat-protocol --example sync_once
```

每个示例都会为测试账号请求全新的二维码登录，打印私有临时 PNG 路径并等待手机端确认。登录成功后二维码即刻删除；凭据仅保存在内存中。`send_text` 仅提交一次并输出回执；`sync_once` 汇总一页同步结果，不保存历史或游标。这些是独立运行的示例，并非连接已有守护进程的客户端。全新登录可能会顶掉其他活跃的桌面端登录。运行 `--help` 不会发起任何网络请求。

发送消息需要传入确切的微信号（用户名），而非好友备注或昵称。任务 ID 并不等同于服务端绝对幂等保证：在重新提交之前，必须先核实状态未决的发送。生产应用程序在提交前必须持久化保存该操作及其时间戳，并对敏感的账号状态进行加密存储。

使用已认证账号发送文本的简短代码示例请参见 [Rust 开发库指南](https://thisnick.github.io/agent-wechat/zh-cn/reference/rust-libraries/#发送单条文本消息)。

## 传输层与受支持的操作

传输层在解码业务消息之前，会对配置的公共服务端证书公钥固定（Pin）、握手 Finished 值、每个 GCM 记录、业务签名（凭据 1）以及业务 GCM 信封进行严格鉴权。不存在任何不安全的对等端或签名绕过设置。仅接受来自已认证响应的路由重定向，且严格限制在受支持的官方域名后缀中。

条件性局域网登录挑战属于实验性功能。通知中的字段 17 是包含非存储风险扩展的 SKBuffer；状态 11 会启动带有本地 IPv4 地址/端口条目、当前登录标识符和全新 16 字节 SKBuffer 的 CGI 22312。临时 TCP 监听器返回大端序的 u32 总长度，后跟包含返回的不透明令牌和字符串 `scene: "xwechat"` 的 JSON 数据。它绝不直接授予授权；常规轮询与最终鉴权依然适用。监听器/令牌状态绝不被序列化，并受取消操作和尝试截止时间的约束。针对签发的登录标识符仅尝试上传提交一次；可重复的轮询路径不会重试不确定的上传。`poll_login_guarded` 允许所有者提供更早的截止时间和非阻塞取消回调。该路径具有离线固件和模拟手机测试覆盖；真实手机/服务端验收仍未经验证。

该 Crate 包含二维码登录、二维码轮询/确认交换、已保存设备手机端确认、受信任的 AutoAuth 续登、NewSync 游标合并、心跳、文本发送、已知联系人和群组查找、联系人引导初始化分页、头像二进制字节及语音分片下载。CDN 模块实现了已认证的路由引导、加密 Range 分片下载、初始纯文本上限为 32 MiB 的普通文件上传、提供的 JPEG 图片变体以及 MP4/缩略图上传。元数据完结器覆盖文件、图片、视频和受限的 SILK 语音留言。内置的便携式 SILK SDK 负责对全新的单声道 16 kHz 音频进行编码并解码传入的语音字节，无需加载官方客户端编解码器。独立的 Core 层负责图片缩放、输入转换、持久化任务、上传回读及不确定发送处理。表情拉取严格限制在经过验证的原始字节 CDN 主机上，带有固定公共 DNS、拒绝重定向、MD5 校验及受限的完整图像解码。

原生普通文件实现当 PKCS#7 密文达到 25 MiB 时选择四阶段的 apply/uploadblock/fin/complete 载体。对于低于 25 MiB 纯文本的 16 字节填充区间，使用参考实现中支持的多阶段策略；未实现参考默认的二进制流 `uploadbig` 载体。高于 25 MiB 的纯文本选择文件类型 7 及最终 AppMsg 字段 6=7。上传和下载根据各自的操作选择器独立选择经过认证的安全 C2C 路由表；上传路由类型 7 与请求的实际文件类型 5 或 7 相互独立。文件类型 7 回读在最终提交前使用 `downloadbig`、精确密文与明文大小、源 MD5 及调用方拥有的 SHA-256 校验。常规图片/视频/语音上传上限保持为 26,214,383 字节。

`NativeClient::reserve_file_upload` 使用单独由调用方持久化的进度 ID 提交一次 CGI 6691。其 `FileUploadReservation` 包含机密令牌资料，带有遮蔽的 Debug 实现及 Drop 时清零机制；请使用上传密钥、源和最终客户端 ID 对其进行保护。`send_file_with_reservation` 发送预留元数据并关联最终的 CGI 222 回执。响应失败绝不能导致发起新的预留或最终发送。现有的无预留 API 保留其原有的报文形式。大文件缓存命中和服务器重试/路由控制会被直接拒绝，而非随意猜测。该路径具有合成架构/传输测试，并且在隔离环境下原生 32 MiB 文件传输助手 PDF 测试获得了接受，具备精确回读、接收方打开及完整本地回写。冷启动 CLI/SDK 字节与 Range 检查通过；接收方导出的 SHA-256 仍不可用。当前验证记录参见 [FEATURES.md](../../FEATURES.md)。

结果将无符号 64 位消息 ID 保留为十进制字符串。未知的命令载荷和联系人字段保留为原始字节。所有写入/发送方法仅提交一次。提交后的响应失败属于不确定结果；服务拥有持久化意图，绝不能盲目重发。

`AccountState` 和 `LoginAttempt` 属于可序列化的**机密状态**，带有遮蔽的 Debug 实现并在 Drop 时清零。请仅将其保存在加密的服务保管库中。切勿从公共 API 返回其序列化表示。可选的 32 字节官方 Profile 密码以及原始 auth/account/network/AX 字段均保存在该受保护状态中。AuthSect 字段 9 仅在原生认证更新门禁开启且该字段存在时才会修改密码。`AccountState::from_baseline_json` 用于转换显式指定的私有独立检查点；它不负责发现官方 Profile 或读取其文件。

测试包含离线合成与已知结果校验。经单独授权的隔离 AMD64 验证在经过认证的会话过期响应后接受了一次受信任的 AutoAuth 续约、一次心跳和一次 NewSync 页面（125 条命令，包含 117 条 AddMsg 记录），使用此 Rust 实现且无需二维码请求、手机提示或消息发送。原始账号/检查点文件保持未变。这是单一测试账号/构建的结果，不代表通用的服务端验收。

多媒体网络方法遵循先前验证的原生方案。扩展的多分片 MP4 上传仍需进行隔离环境的真实验证。群发送、建群、发表情、主动 Push 关联以及长期存活的长连接重连监管器仍处于经验证范围之外。头像字节仍需要在调用方中进行完整的受限图像解码。二维码图片仅做受限的 PNG 头部检查；提供的 JPEG 上传变体会在像素/分配内存限制下进行完整解码。MP4 预处理仅验证容器签名，不做完整的音视频解码。语音完结接受最多 60 秒且编码后不超过 186,010 字节的 format-4 SILK 留言，该上限派生自内置的 16 kbit/s、20 ms 配置并带有 1.5 倍速率余量；内置编码器可从全新 PCM 编码生成。短连接请求在单条 MMTLS 记录中传输，因此大于 32 KiB 的留言会作为单个客户端消息 ID 的连续 `uploadvoice` 偏移量/长度范围提交，最后一段带结束标记，并带上来自更早范围的服务端消息 ID。该多范围路径仅具有离线固件，仍需进行真实环境验证。该模块不进行麦克风录制。

NewSync 的翻页遵循返回与发送的原始游标字节对比；提交的游标采用每个 Key 的无符号最大值合并（或特殊的 -2006 替换）。`wire::decode_add_message` 将字段 6/7 保留为带数字编号的值。在 319 次确切保留的消息关联全部与独立保存的官方 `server_seq` 值匹配后，存在的字段 13 也被暴露为 `server_sequence`。缺失的字段 13 保持为 `None` 而不是零，并且这种关联并不证明任意消息类型均可写入官方数据库。群消息的实际成员发送者在完成独立的信封/成员验证前保持未解析状态。受支持的传入路径为受限的心跳选择器提示（`selector & 7`）随后跟 NewSync scene 7；仅仅处于传输就绪状态的长连接并不能证明账号已完成推送关联。

HTTP 载体为端口 80，传输经认证的 MMTLS 数据。系统 DNS 解析不在严格的端到端截止时间控制内；随后的连接/读取/写入与 HTTP 请求均受时间限制。调用方取消不会停止正在进行的阻塞式发送，因此所有权和完结状态必须由服务 Actor 维持。

在原生 Rust 工作区中运行 `cargo test -p wechat-protocol`。公共固件的许可证和锁定来源参见 `NOTICE`。
