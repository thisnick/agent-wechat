# `wx` CLI 故障排查与应急修复指南 (Troubleshooting)

本文档系统梳理了在使用 `wx` CLI 时可能遇到的各种“不可用”异常场景、根本原因及排查修复方案。

---

## 快速自检工具：一键健康诊断

在遇到任何未知异常时，首先运行 Skill 内置的健康诊断工具：

```bash
packages/skills/agent-wechat/scripts/wx-helper.sh --doctor
```
该命令会自动按顺序检测：
1. 原生二进制是否存在且版本为 1.x；
2. 本地守护进程是否正在监听（`127.0.0.1:6174`）；
3. 是否存在运行中的官方微信冲突进程；
4. 微信主网关的 TCP 80 端口连通性；
5. 当前账号会话就绪状态。

---

## 常见故障分类与解决方案

### 故障 1：`wx` 命令找不到或执行了旧版 Docker 0.x CLI

#### 表现症状
* 终端提示 `command not found: wx`；
* 执行 `wx --help` 显示的是 `up`, `down`, `screenshot`, `a11y` 等已被废弃的 Docker 0.x 命令，执行 `wx --version` 显示 `0.16.0`。

#### 根本原因
* 系统 PATH 中未安装原生 1.x CLI，或者 PATH 中残留了通过全局 npm 安装的旧版 0.x 包。

#### 修复方案
1. **方案 A：使用 npm 安装最新的原生 1.x CLI**：
   ```bash
   npm install -g @agent-wechat/cli@beta
   # 校验版本是否为 1.x
   wx --json version
   ```
2. **方案 B：在本地源码工作区中直接编译构建**：
   ```bash
   pnpm native:build
   # 构建完成后二进制位于 packages/native-client-rust/target/debug/wx
   ```
3. **方案 C：使用显式环境变量覆盖**：
   ```bash
   export AGENT_WECHAT_NATIVE_BINARY=/absolute/path/to/target/release/wx
   ```
4. **方案 D：使用 Skill 提供的封装脚本**：
   ```bash
   packages/skills/agent-wechat/scripts/wx-helper.sh <COMMAND>
   ```

---

### 故障 2：无法连接守护进程（Connection Refused）

#### 表现症状
* 执行任何业务命令（如 `wx status`、`wx messages send`）报错：
  ```text
  Error: Connection refused (os error 111) / Failed to connect to 127.0.0.1:6174
  ```

#### 根本原因
* 后台守护进程未启动，或异常退出导致端口未处于监听状态。

#### 修复方案
1. **拉起后台守护进程**：
   ```bash
   wx daemon start
   wx daemon status
   ```
2. **在前台运行排查详细日志**：
   ```bash
   wx serve
   ```
3. **端口冲突处理**（若 6174 被其他程序占用）：
   ```bash
   # 查看占用 6174 端口的进程
   lsof -i :6174 || netstat -tlpn | grep 6174
   # 更换端口启动
   wx serve --listen 127.0.0.1:8765
   # 并在客户端指定新端口
   wx --server http://127.0.0.1:8765 status
   ```

---

### 故障 3：网络连接失败或 MMTLS 握手超时

#### 表现症状
* 报错 `MMTLS connection closed`、`MMTLS deadline exceeded` 或 `Native connection failed`。

#### 根本原因
* 微信协议要求客户端通过**原始 TCP 直连 `long.weixin.qq.com:80` 和 `short.weixin.qq.com:80`** 执行 MMTLS 双向握手；
* 如果宿主机配置了通用的 HTTP 代理（`http_proxy` / `https_proxy`），而代理服务器尝试将 80 端口当作普通 HTTP 请求解包拦截，会导致 MMTLS 二进制隧道被强行断开；
* 本地防火墙封禁了出站的 80 端口 TCP 流量。

#### 修复方案
1. **取消可能拦截原始 TCP 的局部环境变量代理**：
   ```bash
   unset http_proxy https_proxy all_proxy
   ```
2. **测试底层 TCP 连通性**：
   ```bash
   nc -zv long.weixin.qq.com 80
   nc -zv short.weixin.qq.com 80
   ```
3. **DNS 解析异常修复**：
   若无法解析微信域名，可临时在 `/etc/hosts` 中加入经确认的腾讯云机房公网 IP（或通过 `hosts.xml` 复用官方客户端已缓存的机房 IP）。

---

### 故障 4：官方 Profile 接管失败或报锁冲突

#### 表现症状
* 执行 `wx auth login --official` 报错：
  ```text
  Error: ambiguous profile host redirects / profile locked / database is locked
  ```

#### 根本原因
* 违反了**“单写入者铁律”**：官方 Linux 微信客户端（`xwechat`）仍然在后台悄悄运行，占用了 SQLCipher 数据库文件排他锁；
* 或者宿主机存在多个微信账号的 Profile，系统无法自动断定接管哪一个。

#### 修复方案
1. **彻底杀灭官方微信进程**：
   ```bash
   killall -9 wechat xwechat 2>/dev/null || true
   # 检查确认无存活进程
   pgrep -i "xwechat|wechat"
   ```
2. **显式指定单个确切的账号 Profile 路径**：
   ```bash
   # 直接指定账号所在的 db_storage 父目录，避免多账号歧义
   wx auth login --official=/home/user/.xwechat/xwechat_files/wxid_specific123
   ```

---

### 故障 5：账号会话过期或未登录（Auth Required）

#### 表现症状
* 报错 `auth_required`、`session_expired` 或已读/发送操作返回 401/403。

#### 根本原因
* 距离上次登录时间过长，微信服务端回收了 `AutoAuthTicket`，或者用户在手机端主动点击了“退出 Windows/Mac/Linux 微信”。

#### 修复方案
1. **检查当前会话状态**：
   ```bash
   wx account status
   ```
2. **尝试免扫码重连（若设备信任依然有效）**：
   ```bash
   wx account connect
   ```
3. **若重连失败，重新发起扫码登录**：
   ```bash
   wx auth login --force-qr --qr-output=/tmp/new_qr.png
   # 在图形界面或通过终端工具打开图片扫码
   ```

---

### 故障 6：发送操作超时或退出码 8（Uncertain State）

#### 表现症状
* 命令退出码为 `8`；
* JSON 输出包含 `"outcome": "uncertain"` 和 `"receiptState": "needs_review"`。

#### 根本原因
* 网络在服务端处理完成后、客户端接收确认前发生闪断；无法断定消息是否已送达。

#### 修复方案（严禁盲目直接重发！）
1. **通过回执查询云端确认状态**：
   ```bash
   # 使用输出中附带的 sendId 查询
   wx messages sends get <SEND_ID>
   ```
2. **触发主动增量同步对账**：
   ```bash
   wx sync
   ```
3. 查看最近的历史记录是否已经包含该消息：
   ```bash
   wx messages list <THREAD_ID> --limit 5
   ```
*如果历史记录已存在，说明服务端已入库，无需重发；如果确定未送达且 `sends get` 显示失败，方可重新发起发送。*
