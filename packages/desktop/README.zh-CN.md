# AgentWechat desktop

[English](README.md) | [简体中文](README.zh-CN.md)

该工作区包将 React 客户端构建为 Tauri 桌面应用，并打包了原生的 `wx`、FFmpeg 和 FFprobe 可执行文件。它与 `@agent-wechat/cli` 是独立的：安装 npm CLI 并不会安装该 UI 或桌面运行时。

首次启动时，可选择**“使用此设备” (Use this device)** 或**“连接到其他 Hub” (Connect to another hub)**。桌面连接密钥保存在操作系统的凭据管理器中：macOS 上的 Keychain、Windows 上的凭据管理器 (Credential Manager)，以及 Linux 上的 Secret Service。Linux 桌面需要可用且已解锁的 Secret Service 实现。

应用通过 `wx serve --token-stdin` 传递本地 Hub 密钥。它在原生层代理 API 请求和事件流，而无需将密钥保存在浏览器存储中。内置 CLI 与通过 npm 安装的 CLI 具有相同的凭证行为；两者均不访问桌面系统的 Keyring。独立的 CLI 连接使用其受保护的令牌文件和保存的服务端配置。**“设置 → 导出 CLI 密钥” (Settings → Export key for CLI)** 可为该配置创建受保护的文件。

配置完成后，每当应用打开时本地 Hub 就会启动，包括在界面重连到远程 Hub 时。关闭窗口只会隐藏界面；Hub 和消息通知会继续在后台运行。可以从 macOS 的 Dock 重新打开、重新启动 AgentWechat，或从系统托盘/菜单栏图标选择**“打开 AgentWechat” (Open AgentWechat)**。

选择**“退出 AgentWechat” (Quit AgentWechat)** 将根据 PID、Hub ID 和归属代际请求确切关闭所属的子 Hub 进程。它绝不会接管其他进程，也不会停止远程 Hub。Profile 和加密密钥保存在安装目录之外的应用数据文件夹中。

对于在所选 Hub 上已登录的 Profile，即使窗口隐藏，消息通知也会在原生应用中继续运行。修改**“设置 → 消息通知” (Settings → Message notifications)** 可关闭通知，或使用单个账号的设置仅在此设备上禁用该账号的通知，桌面全局开关优先级更高。当联系人同步就绪后，应用会对已知未静音聊天中的新传入消息发出提醒。它会自动抑制在聚焦窗口中正在阅读的会话、历史记录、传出消息以及重复事件。启动应用时会从当前事件边界开始，不会重放旧提醒。受保护的通知回执中仅保存消息 ID 和事件游标。

引用回复的通知会显示回复的文本内容。如果该文本不可用，预览将显示为“回复”。

在选中该 Hub 的情况下，点击提醒将打开对应的账号和聊天会话。Windows 上的点击在 Toast 弹窗显示期间上报；操作中心激活仍受通知插件平台特性的限制。系统的通知设置和“请勿打扰”同样适用。macOS 和 Windows 的通知递送需要安装版应用的权限许可。

## 应用更新

已安装的应用在启动后以及每隔 6 小时会检查一次 GitHub Releases。**“设置 → 应用更新” (Settings → App updates)** 也提供手动检查入口以及包含预发布版本的选项。预发布构建默认包含预发布版本。检查更新绝不会自动安装或重启应用。点击**“安装并重启” (Install and restart)** 会在停止当前拥有的 Hub 前下载并校验安装包；Profile、保存的连接与凭据均位于安装目录之外。远程 Hub 绝不会被本应用升级。

每次发布都会生成一份 `desktop-updater.json`，其中包含版本号以及各平台专用的 URL 和签名。Tauri 使用内置公钥验证安装包，并要求签名版本匹配。不提供旧版本或相同版本。更新签名与 Apple/Windows 代码签名及 Hub 访问密钥相互独立。macOS 使用公证并装订的 App 归档；Windows 使用最终签名的 NSIS 安装程序；Linux 使用 DEB 安装包，且可能请求系统安装权限。Debug 构建不会自动检查更新。

## 构建

安装适用于你平台的 [Tauri 前置依赖](https://v2.tauri.app/start/prerequisites/)、Node 24 以及指定的 Rust 工具链。然后执行：

```bash
pnpm install --frozen-lockfile
pnpm --filter @agent-wechat/shared build
cargo build --release --locked --manifest-path packages/native-client-rust/Cargo.toml --target YOUR_TARGET -p wx-native
pnpm --filter @agent-wechat/desktop prepare:sidecars --target YOUR_TARGET
pnpm --filter @agent-wechat/desktop app:build --target YOUR_TARGET --bundles YOUR_FORMAT
```

| 目标架构 (Target) | 格式 (Format) |
| --- | --- |
| `aarch64-apple-darwin` | `app` 或 `dmg` |
| `x86_64-pc-windows-msvc` | `nsis` |
| `aarch64-pc-windows-msvc` | `nsis` |
| `x86_64-unknown-linux-gnu` | `deb` |
| `aarch64-unknown-linux-gnu` | `deb` |

媒体处理辅助工具直接从固定的 FFmpeg 静态发布版中下载，并根据已提交的 SHA-256 哈希进行校验。其许可证、上游构建信息及源码链接均随应用一同提供。Windows ARM64 通过 Windows 仿真机制使用 x64 FFmpeg/FFprobe 辅助工具。发布的 Linux DEB 安装包面向 Ubuntu 24.04 或更新的兼容系统。

## 验证与发布

`desktop.yml` 会在原生平台 runner 上为 PR 构建未签名的安装程序。版本发布流程会调用相同的 workflow 并启用签名。有关签名、公证及构件验证，请参见仓库的[发布说明](../../RELEASING.md)。成功的构建或模拟器检查并不代表新操作系统上的实际微信登录或媒体兼容性已经就绪；仍需进行实际安装应用的端到端验收。
