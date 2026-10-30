# `wx` CLI 典型场景自动化操作剧本 (Runbooks)

本文档提供常见生产与开发场景下的标准自动化操作步骤剧本，可直接供 Agent 或 DevOps 脚本参考执行。

---

## 剧本一：从零启动守护进程并完成二维码登录

适用于：全新机器部署、无现成官方微信客户端 Profile、需要通过手机微信扫码建立会话。

### 步骤 1：启动后台守护进程
```bash
wx daemon start
# 确认守护进程正常运行
wx daemon status
```

### 步骤 2：生成并拉取登录二维码
```bash
# 异步提交登录挑战
OPERATION=$(wx --json auth login --strategy=qr --detach | jq -r '.operationId')
echo "当前登录操作 ID: $OPERATION"

# 下载二维码图片到本地临时目录
wx auth qr "$OPERATION" --output=/tmp/wechat_qr.png
echo "二维码已生成至 /tmp/wechat_qr.png，请使用手机微信扫码"
```

### 步骤 3：轮询手机扫码与确认状态
```bash
while true; do
  STATUS=$(wx --json auth operation "$OPERATION" | jq -r '.status')
  echo "当前扫码状态: $STATUS"
  
  if [ "$STATUS" = "approved" ] || [ "$STATUS" = "success" ]; then
    echo "登录确认成功！"
    break
  elif [ "$STATUS" = "expired" ] || [ "$STATUS" = "cancelled" ]; then
    echo "二维码已失效或已取消，需重新发起"
    exit 1
  fi
  
  sleep 2
done

# 校验登录账号就绪状态
wx status
```

---

## 剧本二：原地接管已停止的官方微信 Profile（免扫码）

适用于：Linux 宿主机此前已正常安装并登录过官方 Linux 微信客户端（`xwechat`），希望直接借用官方 Profile 登录态，免去扫码过程。

### 步骤 1：必须彻底停止官方客户端
```bash
# 确保没有官方微信进程在运行 (铁律：单写入者保护)
killall -9 wechat xwechat 2>/dev/null || true
```

### 步骤 2：原地接管并登录
```bash
# 启动守护进程
wx daemon start

# 在位接管现有的官方 Profile
wx auth login --official
```
*手机微信通常会弹出一次“在其他设备上确认登录”的系统提示，点击确认后即可完成秒级免扫码登录。*

---

## 剧本三：各类消息的高可靠自动化发送与回执校验

适用于：告警通知、定期报告、智能客服或 Agent 自动回复。

### 1. 发送带唯一幂等键的纯文本告警
```bash
IDEM_KEY="alert_$(date +%s)_$RANDOM"
wx --json messages send "filehelper" \
  --text "【系统监控】服务器负载过高，请立即检查！" \
  --idempotency-key "$IDEM_KEY"
```

### 2. 发送图文报表与附件
```bash
# 发送每日趋势图
wx messages send "filehelper" --image /data/reports/daily_chart.png

# 发送详细 PDF 审计报告
wx messages send "filehelper" --file /data/reports/audit_2026.pdf
```

### 3. 群聊中精准 `@` 相关负责人
```bash
wx messages send "12345678@chatroom" \
  --text "请值班工程师尽快确认此异常" \
  --mention "wxid_engineer_a" \
  --mention "wxid_engineer_b"
```

### 4. 引用用户此前提出的问题进行回复
```bash
wx messages send "wxid_customer" \
  --text "您咨询的问题已处理完毕，请重新刷新查看。" \
  --quote-message-id "msg_prev_1002" \
  --quote-server-id "6677889900112233"
```

---

## 剧本四：群聊自动化组建与成员生命周期管理

适用于：自动化组建项目协作群、客户服务群、自动拉人与移除离职人员。

### 步骤 1：创建包含多人的工作群聊
```bash
# 初始成员必须在 2 至 40 人之间
ROOM_ID=$(wx --json groups create wxid_user1 wxid_user2 wxid_user3 --name "项目协同突击组" | jq -r '.roomId')
echo "新建群聊 ID: $ROOM_ID"
```

### 步骤 2：设置机器人在该群内的群昵称名片
```bash
wx groups my-name "$ROOM_ID" "智能协同助理"
```

### 步骤 3：成员邀请与移除
```bash
# 邀请新成员进群
wx groups members add "$ROOM_ID" wxid_user4

# 查看当前群成员完整列表
wx groups members list "$ROOM_ID"

# 将某成员移出群聊
wx groups members remove "$ROOM_ID" wxid_user2
```

---

## 剧本五：构建常驻事件监听处理循环

适用于：实时监听新到达的消息、聊天事件，并向后端服务派发。

```bash
#!/usr/bin/env bash
CURSOR_FILE="/tmp/wechat_consumer_cursor.txt"

echo "开始启动微信实时事件监听流..."

# 使用 --watch 持续阻塞监听，并通过 --cursor-file 自动持久化记录游标
wx --json events watch --cursor-file "$CURSOR_FILE" | while read -r line; do
  EVENT_TYPE=$(echo "$line" | jq -r '.type // empty')
  
  case "$EVENT_TYPE" in
    "message.committed")
      MSG_ID=$(echo "$line" | jq -r '.data.messageId')
      SENDER=$(echo "$line" | jq -r '.data.senderId')
      TEXT=$(echo "$line" | jq -r '.data.body // empty')
      echo "[新消息] 来自 $SENDER: $TEXT (ID: $MSG_ID)"
      
      # 可在此处触发业务处理逻辑或调用 Agent 接口
      ;;
      
    "message.recalled")
      RECALLED_ID=$(echo "$line" | jq -r '.data.messageId')
      echo "[消息撤回] 消息 $RECALLED_ID 已被原作者撤回"
      ;;
      
    *)
      # 忽略其他心跳或非核心事件
      ;;
  esac
done
```
