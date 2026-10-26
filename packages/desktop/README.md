# AgentWechat desktop

This workspace package builds the React client as a Tauri desktop app and bundles the native `wx`, FFmpeg and FFprobe executables. It is separate from `@agent-wechat/cli`: installing the npm CLI does not install the UI or desktop runtime.

On first launch choose **Use this device** or **Connect to another hub**. Desktop connection keys stay in the operating system's credential store: Keychain on macOS, Credential Manager on Windows, and Secret Service on Linux. A Linux desktop needs an available, unlocked Secret Service implementation.

The app passes a local hub's key through `wx serve --token-stdin`. It brokers API requests and event streams in the native layer, without placing the key in browser storage. The bundled and npm-installed CLI have the same credential behavior; neither accesses the desktop keyring. Independent CLI connections use their own protected token files and saved server settings. **Settings → Export key for CLI** creates a protected file for that setup.

Once configured, the local hub starts whenever the app opens, including when the interface reconnects to a remote hub. Closing the window keeps the app and its hub in the tray/menu bar. **Quit AgentWechat** requests shutdown of the exact child hub using its PID, hub ID and ownership generation. It never adopts another process, and remote hubs are not stopped. Profiles and encryption keys stay in the app's data folder outside the installation directory.

## Build

Install the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform, Node 24 and the pinned Rust toolchain. Then:

```bash
pnpm install --frozen-lockfile
pnpm --filter @agent-wechat/shared build
cargo build --release --locked --manifest-path packages/native-client-rust/Cargo.toml --target YOUR_TARGET -p wx-native
pnpm --filter @agent-wechat/desktop prepare:sidecars --target YOUR_TARGET
pnpm --filter @agent-wechat/desktop app:build --target YOUR_TARGET --bundles YOUR_FORMAT
```

| Target | Format |
| --- | --- |
| `aarch64-apple-darwin` | `app` or `dmg` |
| `x86_64-pc-windows-msvc` | `nsis` |
| `aarch64-pc-windows-msvc` | `nsis` |
| `x86_64-unknown-linux-gnu` | `deb` |
| `aarch64-unknown-linux-gnu` | `deb` |

Media helpers are downloaded from the pinned FFmpeg static release and checked against committed SHA-256 hashes. Their licenses, upstream build information and source links accompany the app. Windows ARM64 uses x64 FFmpeg/FFprobe helpers through Windows emulation. Release Linux DEBs target Ubuntu 24.04 or newer compatible systems.

## Validation and releases

`desktop.yml` builds unsigned PR installers on the native platform runners. The version-package release calls the same workflow with signing enabled. See the repository's [release instructions](../../RELEASING.md) for signing, notarization and artifact validation. A successful build or simulator check does not establish actual WeChat login or media compatibility on a new operating system; installed-app acceptance remains required.
