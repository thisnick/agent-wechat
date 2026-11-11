# `wx` CLI Troubleshooting and Remediation Guide

This document systematically covers common failure modes when using the `wx` CLI, their root causes, and step-by-step remediation procedures.

---

## Quick Health Diagnostic Tool

When encountering unexpected errors, run the bundled diagnostic check first:

```bash
packages/skills/agent-wechat/scripts/wx-helper.sh --doctor
```

This command sequentially checks:
1. Whether the native binary exists and is version 1.x or later.
2. Whether the local daemon is listening on `127.0.0.1:6174`.
3. Whether conflicting official WeChat desktop processes are running.
4. Outbound TCP port 80 connectivity to WeChat gateway servers.
5. Account session readiness.

---

## Common Failures and Solutions

### Issue 1: `wx` command not found or legacy Docker 0.x CLI executed

#### Symptoms
* Terminal displays `command not found: wx`.
* Running `wx --help` shows deprecated Docker 0.x commands (`up`, `down`, `screenshot`, `a11y`), or `wx --version` prints `0.16.0`.

#### Root Cause
* The native 1.x CLI is not in `PATH`, or an old npm package installed globally is taking precedence.

#### Remediation
1. **Option A: Install latest native 1.x CLI via npm**:
   ```bash
   npm install -g @agent-wechat/cli@beta
   # Verify version is 1.x
   wx --json version
   ```
2. **Option B: Build from local source in repository**:
   ```bash
   pnpm native:build
   # Binary will be placed at packages/native-client-rust/target/debug/wx
   ```
3. **Option C: Explicit environment variable**:
   ```bash
   export AGENT_WECHAT_NATIVE_BINARY=/absolute/path/to/target/release/wx
   ```
4. **Option D: Use the bundled skill wrapper**:
   ```bash
   packages/skills/agent-wechat/scripts/wx-helper.sh <COMMAND>
   ```

---

### Issue 2: Daemon connection refused (`Connection refused 127.0.0.1:6174`)

#### Symptoms
* Commands fail with:
  ```text
  Error: Connection refused (os error 111) / Failed to connect to 127.0.0.1:6174
  ```

#### Root Cause
* The background daemon is not running or exited unexpectedly.

#### Remediation
1. **Start the background daemon**:
   ```bash
   wx daemon start
   wx daemon status
   ```
2. **Run in foreground for diagnostic logs**:
   ```bash
   wx serve
   ```
3. **Handle port conflicts** (if port 6174 is occupied by another process):
   ```bash
   # Check what is using port 6174
   lsof -i :6174 || netstat -tlpn | grep 6174
   # Start with a different port
   wx serve --listen 127.0.0.1:8765
   # Point CLI to the new port
   wx --server http://127.0.0.1:8765 status
   ```

---

### Issue 3: Network connection failed or MMTLS handshake timed out

#### Symptoms
* Errors such as `MMTLS connection closed`, `MMTLS deadline exceeded`, or `Native connection failed`.

#### Root Cause
* The WeChat protocol requires raw TCP communication directly to `long.weixin.qq.com:80` and `short.weixin.qq.com:80` for mutual MMTLS handshake.
* Generic HTTP proxies (`http_proxy` / `https_proxy`) attempting to unpack port 80 traffic as HTTP will corrupt binary MMTLS tunnels.
* Local firewalls blocking outbound TCP port 80 traffic.

#### Remediation
1. **Bypass HTTP proxy environment variables**:
   ```bash
   unset http_proxy https_proxy all_proxy
   ```
2. **Test underlying TCP connectivity**:
   ```bash
   nc -zv long.weixin.qq.com 80
   nc -zv short.weixin.qq.com 80
   ```
3. **DNS resolution check**:
   If domain lookup fails, configure known server IPs in `/etc/hosts` or reuse cached endpoints from `hosts.xml`.

---

### Issue 4: Official Profile takeover failed or lock conflict

#### Symptoms
* `wx auth login --official` fails with:
  ```text
  Error: ambiguous profile host redirects / profile locked / database is locked
  ```

#### Root Cause
* **Violation of the One-Writer Rule**: The official Linux WeChat client (`xwechat`) is still running in the background and holding exclusive SQLite/SQLCipher locks.
* Or multiple profile directories exist, causing ambiguity over which profile to take over.

#### Remediation
1. **Terminate all official WeChat processes**:
   ```bash
   killall -9 wechat xwechat 2>/dev/null || true
   # Verify no processes remain
   pgrep -i "xwechat|wechat"
   ```
2. **Explicitly specify the profile directory**:
   ```bash
   wx auth login --official=/home/user/.xwechat/xwechat_files/wxid_specific123
   ```

---

### Issue 5: Session expired or not signed in (`auth_required`)

#### Symptoms
* Commands return `auth_required`, `session_expired`, or HTTP 401/403.

#### Root Cause
* The `AutoAuthTicket` expired after prolonged inactivity, or the session was revoked from the mobile WeChat client.

#### Remediation
1. **Check session state**:
   ```bash
   wx account status
   ```
2. **Attempt reconnection** (if device trust is still valid):
   ```bash
   wx account connect
   ```
3. **Re-initiate QR login if reconnect fails**:
   ```bash
   wx auth login --force-qr --qr-output=/tmp/new_qr.png
   # Scan image from mobile WeChat
   ```

---

### Issue 6: Send timed out with exit code 8 (`needs_review`)

#### Symptoms
* Command exits with code `8`.
* JSON response contains `"outcome": "uncertain"` and `"receiptState": "needs_review"`.

#### Root Cause
* Network connection dropped after the request was sent but before the server acknowledgement was received.

#### Remediation (DO NOT blindly re-send!)
1. **Check cloud confirmation status via receipt**:
   ```bash
   wx messages sends get <SEND_ID>
   ```
2. **Trigger sync to reconcile**:
   ```bash
   wx sync
   ```
3. **Verify conversation history**:
   ```bash
   wx messages list <THREAD_ID> --limit 5
   ```
*If the message is visible in history, it reached the server. If `sends get` confirms failure, only then re-send.*
