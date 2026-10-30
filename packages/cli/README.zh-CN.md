# @agent-wechat/cli

[English](README.md) | [简体中文](README.zh-CN.md)

`wx` 是原生的 Rust 客户端及前台守护进程。该 npm 包负责安装匹配的发布版可执行二进制文件；它并不在 JavaScript 中实现原生协议。目前已验证 Linux x64/ARM64 与 glibc 2.36+ 环境。发布矩阵同时构建了 macOS ARM64 以及原生 Windows x64/ARM64 候选构件；原生设备账户验收尚在进行中。当前不支持 musl。接管官方客户端 Profile 的功能仅限 Linux。其他系统上的客户端可以使用受支持的 Linux 守护进程所提供的 HTTP API。`wx --json version` 与 `/v1/status` 公开了运行时与账户能力；不支持的官方数据架构会明确报错。

## 安装

在匹配的发布版本发布之后：

```bash
npm install -g @agent-wechat/cli@VERSION
wx --json version
```

在原生 Beta 测试期间，请使用明确发布的版本号（例如 `1.0.0-beta.0`）或 `@agent-wechat/cli@beta`。在正式晋级前，稳定的 `latest` 渠道仍保留旧版容器 CLI。在假设 Beta 版已发布前，请先查看 Beta 发布说明。

显式 npm 安装会通过经验证的 HTTPS 下载指定版本的构件资产。构件的大小与 SHA-256 哈希必须与 npm 包内置的 `native-release.json` 完全一致，随后本地身份校验必须确认相同的版本、原生后端、Rust 目标架构以及服务提供能力，才会进行原子安装。常规命令调用绝不会自动下载缺失的运行时。清单缺失、不受支持的平台、损坏的构件或错误的后端均会被视为错误。

支持禁用生命周期脚本的安装方式：

```bash
npm install -g --ignore-scripts @agent-wechat/cli@VERSION
wx-native-install
```

对于离线部署，请将发布版二进制文件及其对应的 `wx-vVERSION-checksums.json` 准备在同一个本地目录中，然后运行：

```bash
AGENT_WECHAT_NATIVE_ARTIFACT_DIR=/absolute/release-assets wx-native-install
```

离线清单必须与 npm 包内置清单一致。或者，你也可以自行提供可信的可执行文件并设置 `AGENT_WECHAT_NATIVE_BINARY=/absolute/path/to/wx`；启动器将验证其原生身份和版本，而不是从网络下载。该显式覆盖信任操作员提供的二进制字节，且不假定其与官方发布资产完全相同。设置 `AGENT_WECHAT_SKIP_NATIVE_INSTALL=1` 可在镜像或打包配置期间延迟安装；在提供运行时前，命令保持不可用。请勿禁用 TLS 验证。

OpenClaw 插件使用已安装的全局原生 `wx` 或配置的绝对路径可执行文件。它绝不下载二进制文件，也不依赖其插件安装器执行 npm 生命周期脚本。对于托管进程，建议直接指定实际的发布版二进制路径以避免额外的启动器进程；npm 启动器同样会转发参数、标准流、退出码及终止信号。

## 配置与存储

```bash
wx config show
wx serve --server-config /absolute/server/config.toml
```

默认服务端配置文件路径为 `$XDG_CONFIG_HOME/agent-wechat/server/config.toml` 或 `~/.config/agent-wechat/server/config.toml`。持久化根目录默认为 `$XDG_DATA_HOME/agent-wechat` 或 `~/.local/share/agent-wechat`。可以通过 `wx serve --data-dir PATH` 及环境变量 `AGENT_WECHAT_DATA_DIR` 进行覆盖。服务端配置可选择 `listen`、`dataDir` 和 `tokenFile`；相对路径基于配置文件所在位置解析。文件路径本身不包含凭证。

初始工作流程可在原地接管现有的官方 Profile。请先关闭官方客户端，启动 `wx serve`，然后在另一个终端中执行：

```bash
wx auth login --official
# 或在守护进程主机上指定特定路径：
wx auth login --official=/absolute/official-profile
```

守护进程会绑定已存储的身份凭证，尝试使用已保存的设备信任进行登录，并在认证通过后派生出经过验证的数据库密钥。在该守护进程拥有这些文件期间，官方客户端必须保持关闭。

使用不透明的消息 ID 以及历史记录或验证回执中的精确十进制字符串 Server ID 来撤回自己发送的消息：

```bash
wx --account example messages recall MESSAGE_ID --server-id SERVER_ID
wx --account example operations get OPERATION_ID
```

使用 `--detach` 可立即返回持久化操作对象。在提交撤回请求前会先记录私有回执；同一目标使用稳定的默认幂等键。不确定的结果将以退出码 8 结束，必须进行检查，绝不能自动重新提交。撤回资格由微信服务端决定，停止监听器不会取消已提交的撤回。已撤回的消息将作为占位符保留；其多媒体内容无法通过守护进程获取。原始日志、缓存文件和备份均被保留。

此外还提供更底层的注册与实验性 Profile 复制功能：

```bash
# 仅对已显式停止/静止的 Profile 使用；切勿共享写入者。
wx store attach --account example --path /absolute/official-profile --source-stopped
wx store import --account copied --source /absolute/stopped-profile --source-stopped
wx store export --account copied --destination /absolute/new-export --source-stopped
```

直连模式将官方 Profile 数据保留在选定路径下，而仅属于守护进程的状态保存在 `dataDir` 下。全新的 Profile 创建、导出以及将文件归还给官方应用不属于初始支持的接管范围。导入/导出复制相同格式的数据；它们不会转换数据库架构或刷新登录信任。使用 `--help` 查看已实现的命令界面。切勿隐式复制或挂载正在运行的生产 Profile。

## 构建与本地验证（无需部署）

在仓库根目录下执行：

```bash
pnpm --filter @agent-wechat/cli... build
pnpm --filter @agent-wechat/cli test
pnpm native:build
pnpm native:test
pnpm native:release:build
node scripts/native-package-smoke.mjs artifacts/native
# 将 VERSION 替换为构建输出的准确构件版本号：
python3 scripts/native-cli-smoke.py artifacts/native/wx-vVERSION-linux-x64-gnu
python3 scripts/native-cli-fixture-test.py artifacts/native/wx-vVERSION-linux-x64-gnu
python3 scripts/native-export-smoke.py artifacts/native/wx-vVERSION-linux-x64-gnu
```

源码工作区的 postinstall 阶段会延迟发布版安装；请从 target 目录直接运行 Rust 二进制文件，或使用显式的受信任可执行文件覆盖。本地发布构建脚本会测试 Rust 工作区、验证可执行文件身份并创建带版本号的构件；它不会发布、部署或登录账号。包冒烟测试会将实际的 npm tarball 进行打包/解包，并从本地构件安装到临时目录中进行验证。

部署示例和专门的发布准备流程请参见 [deploy/native](../../deploy/native/README.md)。原先的容器 CLI 和 Wechaty 包已退役；详情请参阅[迁移指南](https://thisnick.github.io/agent-wechat/setup/migrate/)。

## 辅助脚本

`native-install.mjs` 在 npm 安装期间运行，或在离线/禁用脚本安装时通过 `wx-native-install` 调用。它会选择主机架构、校验 npm 内置哈希并在原子安装前验证可执行文件的身份。`native-launcher.mjs` 仅负责运行已安装的可执行文件。

`native-check-package.mjs` 是打包与发布前的检查门禁。它会校验版本、源码、目标架构以及校验和元数据；在发布前还要求两种架构的构件均存在且与包清单匹配。本地打包冒烟测试可使用单一架构片段；但正式发布必须具备完整的构件矩阵。这些辅助脚本属于现有的 CLI 包，而非额外的 npm 包。

候选平台验证及剩余的 Profile 边界记录在[验收清单](../../PLATFORM-ACCEPTANCE.md)中。Windows 可执行文件命名为 `wx.exe`；npm 启动器会根据操作系统与 CPU 选择对应的构件。原生的 ARM64 Node 安装会选择原生的 Windows ARM64 可执行文件。
