# `wx` CLI Automation Runbooks

This document provides standardized runbooks for common production and development workflows, suitable for AI agents and DevOps scripts.

---

## Runbook 1: Start Daemon and Complete QR Code Sign-In

Applicable to: Fresh deployments, standalone installations without an official profile, or environments requiring mobile QR scan authentication.

### Step 1: Start Background Daemon
```bash
wx daemon start
# Verify daemon is running
wx daemon status
```

### Step 2: Initiate Challenge and Fetch QR Code
```bash
# Start asynchronous login challenge
OPERATION=$(wx --json auth login --strategy=qr --detach | jq -r '.operationId')
echo "Active login operation ID: $OPERATION"

# Download QR code image to local temporary directory
wx auth qr "$OPERATION" --output=/tmp/wechat_qr.png
echo "QR code generated at /tmp/wechat_qr.png. Scan with mobile WeChat."
```

### Step 3: Poll Confirmation Status
```bash
while true; do
  STATUS=$(wx --json auth operation "$OPERATION" | jq -r '.status')
  echo "Current login status: $STATUS"
  
  if [ "$STATUS" = "approved" ] || [ "$STATUS" = "success" ]; then
    echo "Login confirmed successfully!"
    break
  elif [ "$STATUS" = "expired" ] || [ "$STATUS" = "cancelled" ]; then
    echo "QR code expired or cancelled. Please re-initiate."
    exit 1
  fi
  
  sleep 2
done

# Verify account readiness
wx status
```

---

## Runbook 2: In-Place Takeover of an Official WeChat Profile (Bypasses QR Scan)

Applicable to: Linux desktop environments where the official Linux WeChat client (`xwechat`) has already been installed and signed in.

### Step 1: Stop Official WeChat Client Completely
```bash
# Ensure no official client processes are running (One-Writer Rule)
killall -9 wechat xwechat 2>/dev/null || true
```

### Step 2: Take Over Profile and Sign In
```bash
# Start background daemon
wx daemon start

# In-place takeover of existing profile
wx auth login --official
```
*A system confirmation prompt will appear on your mobile WeChat. Tap confirm to sign in instantly without scanning a QR code.*

---

## Runbook 3: Reliable Message Sending and Receipt Verification

Applicable to: Alerts, recurring reports, bot responses, or agent message dispatch.

### 1. Send Text Alert with Idempotency Key
```bash
IDEM_KEY="alert_$(date +%s)_$RANDOM"
wx --json messages send "filehelper" \
  --text "[System Alert] High CPU utilization detected. Please investigate." \
  --idempotency-key "$IDEM_KEY"
```

### 2. Send Images and Reports
```bash
# Send chart image
wx messages send "filehelper" --image /data/reports/daily_chart.png

# Send PDF report
wx messages send "filehelper" --file /data/reports/audit_2026.pdf
```

### 3. Mention Specific Engineers in a Group Chat
```bash
wx messages send "12345678@chatroom" \
  --text "On-call engineers, please confirm this incident." \
  --mention "wxid_engineer_a" \
  --mention "wxid_engineer_b"
```

### 4. Send Quoted Reply
```bash
wx messages send "wxid_customer" \
  --text "Your reported issue has been resolved. Please verify." \
  --quote-message-id "msg_prev_1002" \
  --quote-server-id "6677889900112233"
```

---

## Runbook 4: Automated Group Chat Creation and Member Management

Applicable to: Project team setup, customer channel creation, and member onboarding/offboarding.

### Step 1: Create Group Chat
```bash
# Initial members must be between 2 and 40
ROOM_ID=$(wx --json groups create wxid_user1 wxid_user2 wxid_user3 --name "Incident Response Team" | jq -r '.roomId')
echo "Created group room ID: $ROOM_ID"
```

### Step 2: Set Bot Group Nickname
```bash
wx groups my-name "$ROOM_ID" "Ops Assistant Bot"
```

### Step 3: Add and Remove Members
```bash
# Add new member
wx groups members add "$ROOM_ID" wxid_user4

# List current group members
wx groups members list "$ROOM_ID"

# Remove member from group
wx groups members remove "$ROOM_ID" wxid_user2
```

---

## Runbook 5: Continuous Event Processing Loop

Applicable to: Real-time message consumption, event routing, and agent triggers.

```bash
#!/usr/bin/env bash
CURSOR_FILE="/tmp/wechat_consumer_cursor.txt"

echo "Starting WeChat real-time event watcher..."

# Block and stream events, persisting cursor to file
wx --json events watch --cursor-file "$CURSOR_FILE" | while read -r line; do
  EVENT_TYPE=$(echo "$line" | jq -r '.type // empty')
  
  case "$EVENT_TYPE" in
    "message.committed")
      MSG_ID=$(echo "$line" | jq -r '.data.messageId')
      SENDER=$(echo "$line" | jq -r '.data.senderId')
      TEXT=$(echo "$line" | jq -r '.data.body // empty')
      echo "[New Message] From $SENDER: $TEXT (ID: $MSG_ID)"
      
      # Route to agent or business logic here
      ;;
      
    "message.recalled")
      RECALLED_ID=$(echo "$line" | jq -r '.data.messageId')
      echo "[Recall] Message $RECALLED_ID was recalled by author"
      ;;
      
    *)
      # Ignore other events
      ;;
  esac
done
```
