# deepseek-harness-extension 设计与实施方案（评审稿）

> 状态：待评审。本文档是开发前的设计交付物，确认后按 §9 里程碑实施。
> 参考实现：`packages/openclaw-extension`（`@agent-wechat/agent-wechat` 3.0.0-beta.1）与 `docs/src/content/docs/openclaw/` 8 篇文档。
> 目标目录：`packages/deepseek-harness-extension`。所有行号引用基于 main@`8735aac2` 与本机 DSH 0.2.2-rc.1 安装。

---

## 0. 目标与非目标

**目标**：为 DeepSeek Harness（DSH）提供一个与 openclaw-extension 同等功能的 WeChat 插件——同一 `wx` 原生守护进程协议（`packages/shared`），接入消息、群聊、媒体、已读、撤回、贴纸、登录、通讯录、全账号归档、语音/会议通话与 owner 工具，并以 DSH 的 Agent/Session/Tools 原语承载 OpenClaw 的 channel/session/tool 语义。

**非目标（一期不做）**：
- 视频通话（OpenClaw 后端同样不支持，`calling.mdx:339-342`）。
- gateway facade HTTP 入口默认开启（OpenClaw 亦默认关闭；二期用 `ctx.webServer` + 本地鉴权实现）。
- Web GUI 中的微信聊天面板（GUI 只是管理面；微信对话进入独立 WeChat Agent 会话）。

**验收原则**（贯穿全文的安全不变量，违反即缺陷）：
1. 一切默认关闭：无 `dmPolicy`/`groupPolicy` 配置时不回复任何人（`access.mdx:8-38`，`setup.mdx:69`）。
2. 只消费已提交（committed）入站消息；重启最多追赶 24h/100 条（`README.md:69-113`，`native-monitor.ts:111-132`）。
3. 访问控制先于媒体下载与转写（`AGENTS.md:114-116`）；被拒绝消息的媒体绝不下载/转写/进模型。
4. `historyLimit` 默认 50、最大 200、0 关闭；撤回消息从上下文剔除（`access.mdx:57-74`）。
5. 发送/撤回使用稳定幂等 key；不确定结果返回 `needs_review`，绝不自动重发（`tools.mdx:155-160`）。
6. read ≠ 理解；`needs_review` 不触发自动重发（`read-receipts.mdx:45-50`）。
7. 工具执行者的权限来自 `ToolExecutionInput.agent`（`dsh-tools/.../contracts.d.ts:175-179`）反查插件路由表，**绝不**信任模型参数里的 sessionKey/账号/owner 标志。
8. archive/owner/通话工具默认对一切 Agent 不可见，除非配置显式授权（`tools.mdx:125-153`）。

---

## 1. 宿主能力对照（OpenClaw → DSH）

| OpenClaw 机制 | DSH 现状 | 方案 |
| --- | --- | --- |
| `registerChannel`（direct/group、media、reply、voice-note） | Host 无 channel/`im` 服务；IM 工具仅作用于当前 GUI IM 会话，不能订阅入站 | 插件自建 `WeChatBridge` host service + 路由注册表（§3） |
| 会话模型（main + `dmScope: per-peer` 等） | `ctx.agents.create/resume(CreateAgentOptions)`，`sessionId` 由调用方提供（`dsh-agent/lib/types/index.d.ts:49-106,111-130,292-300`） | 每个微信线程一个独立持久 Agent；`routeKey→sessionId` 映射存 `ctx.storageDomain`（§3.2） |
| 会话驱动（OpenClaw agent run） | `agent.send/followup/steer/inject`（`runtime-types.d.ts:203-236`）；inbox/pre-step 事件（`:290-339`） | 入站消息 `followup(UserMessage)` 唤醒；闲时逐出（dispose），再消息 `resume`（§3.4） |
| 回复采集（OpenClaw reply dispatcher） | `ctx.sessionQuery` 观察已提交 `assistant/message` 与 `turn/end` | 只消费已提交事件，绝不拼接 process-local transient 流（§5） |
| `tools.alsoAllow` / plugin 工具目录 + `tool_search` | `ctx.tools.register(definition,{when})`、`restrict()`、`guard()`（`dsh-tools/lib/types/index.d.ts:29,50,58,136`） | 工具在 Agent `setup` 内按配置注册；`when` + execute 双重作用域校验（§4） |
| `runtime.tts.textToSpeechTelephony`、realtime transcription、meeting runtime | **无宿主 TTS/telephony/realtime STT**；仅 experimental `speechToText.transcribe(spec,signal)`（`dsh-experimental-speech-to-text/lib/types/index.d.ts:20-90`） | 插件自带 `SpeechProvider` 接口 + v1 OpenAI 适配器；缺失时明确 unavailable，fail closed（§6） |
| secrets / SecretRef | `ctx.credentials`（配置存 `tokenCredentialRef` 引用，不存明文） | 所有 token/apiKey 走 credentials（§7） |
| `consumerStateDir` | `ctx.storageDomain.open()` | 同语义：cursor/journal/去重/outbox 持久化（§3.3、§5.2） |
| `core.mediaUnderstanding.transcribeAudioFile` | experimental STT provider registry（可选依赖） | `SpeechProvider.transcribe`；不可用时语音消息体注入 `[Voice transcription unavailable]`（`native-history.ts:12-18` 同语义） |
| `registerHttpRoute({auth:'gateway'})`（facade） | `ctx.webServer.register()` + 本地 token 鉴权 | 二期；一期 facade 配置存在但默认关，开启即报"未实现" |
| 定时/后台 | `ctx.schedule`/`ctx.timer`/`ctx.jobs` | 读泵、追赶、媒体 worker、outbox 冲洗 |

已核实的 DSH 事件面：`agent/created|disposed|status`、`agent/inbox/inserted|claimed|discarded`、`agent/pre-step`（waterfall，可替换 messages）、`agent/assistant-stream`（end=已提交 assistant/message 或 attempt）、`agent/turn-stopping`、session 侧 `assistant/message`、`turn/end`（reason: completed/aborted/blocked/error/max-tokens/interrupted/forked）。

---

## 2. 总体架构

```
wx daemon (复用，协议在 packages/shared)
   │  HTTP+SSE, token scopes read/media/send/login/read-state/calls/admin
   ▼
┌─ native 层（复用 @agent-wechat/shared）────────────────────────┐
│ native host 生命周期 managed/external、native-client、        │
│ native-calls、native.generated（消息/联系人/通话协议类型）      │
└───────────────────────────────────────────────────────────────┘
   ▼
┌─ bridge 层（移植 native-monitor.ts 状态机 + DSH 适配）─────────┐
│ committed-live-only 消费、cursor/journal、追赶(24h/100)、      │
│ access admission（先于一切媒体）、媒体管线、read pump、撤回    │
└───────────────────────────────────────────────────────────────┘
   ▼                          ▲ 回复
┌─ routing 层 ────────────┐  ┌─ delivery 层 ─────────────────┐
│ routeKey→sessionId 映射  │  │ 观察 committed assistant/     │
│ create/resume/dispose    │◄─│ message + turn/end → outbox   │
│ 会话上下文注入(历史追赶) │  │ (幂等key/needs_review) → send │
└──────────────────────────┘  └─────────────────────────────────┘
   ▼
┌─ tools 层（17 个工具，scope=exec.agent 反查路由表）────────────┐
│ 当前聊天 7 + owner 3(contacts/contact_request/group_info) +    │
│ login + archive 5 + call（默认 deny，配置授权）                │
└───────────────────────────────────────────────────────────────┘
   ▼
┌─ calls 层（协议复用，语音运行时新写）──────────────────────────┐
│ bidi: RealtimeVoiceProvider；agent: Stt+AgentTurn+Tts，        │
│ consultation 独立会话 + per-contact/per-call 记忆              │
└───────────────────────────────────────────────────────────────┘
```

**入口与 manifest**（已对照 cordis-plugin-development `references/host-plugin.md:7-77` 核实）：
- package.json 声明 `"dsh": { "bundle": { "patch": "./cordis.patch.yml" } }`；patch 内容 `insert: [{ id, name, config: {} }]`；正式 bundle 另含 `locales/{en-US,zh-CN}.json` 与 icon（≤256 KiB）。
- 入口 `export function apply(ctx, config)`；`inject = ['tools','agents','sessions','attachments','storageDomain','jobs','schedule','timer','webServer','connection','remoteAccess']`。
- `Config` 用 `@deepseek-ai/schemastery`（`z.object`，参照 `dsh-llm-retry/lib/types/index.js:7-15`）；token 等秘密只存 credential 引用（经 `ctx.credentials.modifyRecord()` 管理），不进 Config；配置编辑走 `ctx.get('configEditor').edit(...)`，热更新字段用 `.volatile()`。

一切注册归属 ctx effect。stop 顺序（对齐 `index.ts:28-68` 并显式化）：停入站/递增 generation → calling 先停 → facade 拒新请求并 abort 流 → abort inbound/read/outbox 并等待 quiesce → 取消 timer/订阅/projection/工具 disposer → 关闭 attachments/storageDomain → managed daemon 有界 stop 超时 SIGKILL（external 仅断开）→ 清内存 prepared media（保留 durable journal，重启从 cursor/custody/outbox 恢复）。

**不设全局 runtime 单例**（弃用 OpenClaw `runtime.ts:1-14` 的 PluginRuntime holder），改为显式 `HostRuntime` 注入，便于多账号与测试隔离。

---

## 3. 消息接入与会话路由

### 3.1 消费状态机（移植 `native-monitor.ts`）
- `committed-live-only` + account/cursor/identity 校验（`:111-132`）；重启追赶窗口 24h、上限 100 条、`historyLimit`（`:348-424`）；read pump 按 `readPolicy: on_inbound|never|on_model_dispatch`（`:426-435`，`read-receipts.mdx:8-16`）。**实现现状**：native 协议无已读回执端点（`native-client.ts` 无 read/markRead），`readPolicy` 为保留配置、当前无效果；read pump 待 daemon 提供已读 API 后落地。
- 派发前完成：admission（DM policy/allowlist、群 sender gate + 真实 mention）、媒体准备（异步下载/转写 worker×3，`native-history.ts:23`）、撤回剔除、群历史上文（`:177-200`）、needs_review 复核（`:202-340`）。
- 入站 UserMessage：`id = wechat-native:<account>:<serverId>:<hash(chatJid)>`（稳定、可持久去重）；`source` 携带结构化线程元数据（account、chatJid、senderId、mention、messageType），**只放可信元数据**，不解析正文猜线程（`dsh-llm/lib/types/message.d.ts:75-98,121-146`）。**实现修正（端到端联调发现）**：id 必须按消息唯一——`id = wechat-native:<account>:<serverId>:<hash(chatJid)>:<hash(serverMessageId)>`。Agent 收件箱（`agent/inbox/spliced`）拒绝两条同 id 的 pending 消息（"already pending"），按线程生成 id 会让同聊天第二条消息永久派发失败；另 followup 对象必须是完整 UserMessage 形状（`{id, role:'user', content:[{type:'text',text}], source:{kind:'user'}}`），裸字符串 source 会让会话日志在下次 resume 时报 "format v4 message requires a producer-owned source kind" 损坏。
- 原生 dispatch proof：以 `agent/inbox/claimed` + 追赶 journal 的双记录代替 OpenClaw 的 prepared-boundary hook（`native-prepared-boundary.ts:21-55` 不照抄）。

### 3.2 路由注册表
- `routeKey = <account>:<chatJid>`；`sessionId = wht-<accountSlug>-<hash(routeKey)>`（稳定可重建）。
- 持久状态统一走 `ctx.storageDomain.open(spec)`（`global` + `table(name){get/entries/put/delete/update}` + `close()`）；表：`accounts`、`threads`（routeKey→sessionId/revision/policy 指纹）、`journal`、`custody`、`outbox`、`recalls`、`config_generation`。不自定义 SessionEvent（practices.md 禁令）。注意：`ctx.sessions.create()` 裸 Session 不保证持久，持久会话必须走 Agent 生命周期（`agents.create/resume`）。
- 会话创建：`ctx.agents.create({ sessionId, meta:{ cwd, origin? }, agentOptions, setup })`；`setup` 内注册该线程 scoped 工具、prompt section（WeChat 线程说明 + 当前 chat 上下文）、`restrict`。恢复：`resume({ resumeSessionId, setup })`。

### 3.3 会话上下文注入（历史追赶）
历史/追赶消息**不伪造为已提交 seed events**，而是随首条触发消息一起，以 `form: notice/relay` 的上下文 UserMessage 批量注入（对齐 OpenClaw "history as context" 语义）。决策理由：seed 需要精确的 `inheritedEventCount` 前缀语义，而追赶内容本质是"备注性上下文"；followup 注入保持 session log 真实性。

### 3.4 生命周期与隔离
- 每线程独立 Agent；**绝不**投递到当前 GUI 会话；owner consultation、通话会话同样独立（§6.3）。
- 并发模型：DSH inbox 天然 per-agent 串行 turn；多线程=多 Agent 并行。
- 闲时逐出：活跃 Agent 上限（默认 16）+ 闲置 N 分钟（默认 30min）`handle.dispose()`；dispose 会从 store 移除 session 但保留持久日志（`index.d.ts:145-148`），下条消息 `resume` 无损恢复。路由表记录 revision 防 resume 竞态。timer/schedule 唤醒 Agent 必须用 `followup()`（`inject()` 只注入上下文、不保证唤醒）。
- agent/pre-step 用于：追加入站批量合并（coalesce 邻近消息为一个 turn）、撤回再剔除；modelSelection 已冻结，不做模型改动。

---

## 4. 工具层

17 个工具全部移植（`openclaw.plugin.json:4-22`），名称与参数保持一致，降低文档/用户迁移成本。注册位置：宿主级注册 + 每 Agent `setup` 中按配置决定可见性（等价 OpenClaw `tools.alsoAllow`），运行期 `when(ToolActivationContext)` 决定步内参与（`contracts.d.ts:109-119`）。

| 工具 | 作用域 | 授权 |
| --- | --- | --- |
| `wechat_history/search/message/media` | 当前线程（routeKey 严格匹配） | 随线程 Agent 自动可用 |
| `wechat_recall`、`wechat_send_share` | 当前线程 | 随线程 Agent；send 需 token `send` scope |
| `wechat_stickers` | 账号 | 仅 owner 绑定会话 |
| `wechat_login` | 账号 | 配置授权（owner-facing agent）+ token `login` scope |
| `wechat_contacts`/`wechat_contact_request` | 账号 | owner 绑定会话 + token `read`/`admin`（`tools.mdx:33-43`） |
| `wechat_group_info` | 账号 | owner 绑定 + `read`（`tools.mdx:115-123`） |
| `wechat_archive_threads/search/history/message/media` | 全账号 | 默认 deny；配置显式授权（`tools.mdx:125-153`） |
| `wechat_call` | 通话控制 | 配置授权 + calls 权限（§6） |

**owner 绑定**：`ownerAllowFrom`（精确 wxid 列表）+ 会话级判定——该 Agent 的 routeKey 是 owner 的 DM 且 senderId 精确匹配。OpenClaw 的"CLI backend 缺 owner 标记"问题在 DSH 不存在：每个线程独立 Agent，`exec.agent.id → 路由表` 即可信身份；模型无法伪造（工具 schema 不暴露任何账号/owner 参数）。

**幂等与不确定结果**：所有 mutation（send/recall/share/contact 操作/通话动作）携带稳定 `idempotencyKey`；outbox 未确认前重放同 key 不二次发送；不确定结果返回 `needs_review`（对齐 `native-monitor.ts:202-340` 的回复幂等语义）。

**工具输出**：声明 `output: ToolOutputDefinition`（契约强制，`contracts.d.ts:23-36`）；图片经 `ctx.attachments.saveImage/readImage` 进入模型可见内容；媒体文件给路径 + file reference。

---

## 5. 回复投递（delivery）

1. **采集**：订阅 `session/event`（post-commit，observer 失败不回滚）收集已提交 `assistant/message`；只消费 `turn/end` 已闭合的提交内容（reason 白名单：completed；aborted/error/max-tokens/interrupted 的尾巴不入发送集）。用 `ctx.sessionProjections.register({key,stateSchema,init,apply,stateVersion})` 维护"已消费 offset"读模型（`apply` 必须纯同步、由每个 committed event 驱动），重启后按 offset 补偿。**不**以 `agent.status==='idle'` 当回复完成（多个输入共享 running 区间）；不使用 deprecated 的 `eventAt/snapshotEvents/ownEvents`。
2. **映射**：文本→文本消息；内容块中的本地文件→媒体消息（大小上限对齐 README `:188-214`）；`replyTo` 用存储消息 stored ID 并校验同 account/chat（`tools.mdx:84-103` 语义由发送侧保证）。
3. **outbox 与可靠发送**（照抄 `native-send.ts:7-27,45-91` 语义）：文本只 POST 一次；失败按原 idempotency key 调 `sendByKey` 恢复，最多轮询 65 秒，不自动重发；回读需精确匹配 message/account/thread/direction/serverId 才 `verified`，否则 `needs_review(retryable:false)`。outbox 表持久化 key/目标/payload/状态（pending/sent/needs_review/failed），重启后扫描未确认项恢复；媒体先于文本（发送顺序约束）。
4. **媒体任务**：`wechat_media` 的 pending 状态机持久化；重启后可取回；不自动重发媒体消息。

---

## 6. 通话（calls）

> 对等口径：OpenClaw README（`:55-67`）自身声明 calling 的电话集成验证不完整。本插件将 calls 作为独立 feature flag 模块（默认关闭），完成 §8 验证矩阵第 5/7 项及通话专项用例前，不宣称"已验证对等"。

### 6.1 配置面（字段与 OpenClaw 对齐）
`calls.voice.{enabled,mode,agentId,inboundPolicy,inboundAllowFrom,outboundPolicy,outboundAllowTo,outboundToolAllowFrom,maxDurationSeconds,mode: bidi|agent,transcription,realtime.*,toolPolicy,allowConsultSessionPersistence,sessionScope,consultation.*}` + `calls.video{enabled:false}` + conferences（groups 精确/`*` 规则、autoJoin、autoJoinAllowFrom、ownerInviterToolPolicy、toolPolicy；精确规则整体替换通配，不合并——`calling.mdx:111-160`）。
已裁决的文档冲突：inbound allowlist 支持 `['*']`（实现依据 `calls-config.ts:140` `names(value.inboundAllowFrom, true)`）；outbound/tool/owner 列表一律精确 ID 不接受 `*`（`calling.mdx:264-268`）。daemon 硬上限 20 分钟照实记录（`calling.mdx:262`）。

### 6.2 语音运行时（新写，接口先行）
```
interface SpeechProvider {           // 插件自带，v1: OpenAI 适配器
  stt?:      { createSession(opts): RealtimeSttSession }   // agent 模式实时转写
  tts?:      { synthesize(text, opts): Promise<{audio, sampleRate, format}> }
  realtime?: { createBridge(...): BidiVoiceBridge }        // bidi 模式
}
```
- **bidi 模式**：RealtimeVoiceProvider（v1 OpenAI Realtime；模型默认 `gpt-realtime-2.1`，支持 `gpt-live-1*` 语义差异按 `calling.mdx:273-277` 记录）。打断=清空排队音频+上报播放位置。
- **agent 模式**：移植 `calls-agent.ts` 管线，参数照抄——PCM16/24kHz、coalesce 200ms、语音门限 rms 650/peak 2500/4 帧/10 帧/300ms 冷却（`:60`）、40ms 帧/最多 200ms ahead 的发送 pacing（`:84-95`）、TTS 上限 180s（`:114`）、MAX_TURN_CHARS 16k、打断即丢弃 late agent/TTS 结果且不重试动作（`:118-122`）。
- **fail closed**：provider 未配置/未连通时，calls 状态 `unavailable`，不接不答（对齐 `calling.mdx:103-105`）。不依赖宿主 experimental STT 包（若宿主存在可作为 STT 备选 source）。

### 6.3 consultation 与通话记忆
- 会话：每个通话独立 consult Agent；`toolPolicy: none|safe-read-only|owner`；持久化进微信会话需显式 `allowConsultSessionPersistence: true`（开启后默认 safe-read-only）。
- owner 通话咨询三重条件照抄（`calling.mdx:200-213`）：voice.toolPolicy=owner + 该群 toolPolicy=owner + ownerAllowFrom 精确条目；信任主体是"整群"而非个人发言者；权限只降不升，升权需新通话。
- 记忆（移植 call-memory）：per-contact/per-call 会话键；每通被接听电话生成 2–4 句总结；下一通提供最近 3 份总结 recap（默认关）；`recentContext`（分享最近聊天）以 call-memory.mdx 为准默认关，实现期以代码为准核正。总结持久化在 DSH 侧 storageDomain，不落明文音频（本插件不录音，`calling.mdx:357-359`）。
- 会议：混合音频不识别发言人身份；roster（入会/离会/名单）来自 cached contacts，仅用于寒暄与状态播报，不授权（`calling.mdx:182-198`）。

---

## 7. 配置与凭据

Config schema 保留 OpenClaw 字段语义（`types.ts:15-41`），两处改动：
- `native.token` → `native.tokenCredentialRef`（经 `ctx.credentials` 解析；managed 模式 token 经管道传子进程）。
- 新增 `speech.{stt,tts,realtime}.provider + credentialRef` 与 `transcription.credentialRef`。

结构：`enabled`、`native{mode:managed|external,accountId,endpoint|executable/serverConfig/dataDir/listen,consumerStateDir}`、`dmPolicy/groupPolicy/allowFrom/groups/per-group覆盖`、`readPolicy/historyLimit`、`tools{archive,owner,call,...per-agent授权}`、`calls{...}`、`facade{enabled:false}`。默认全关；校验失败拒绝启动而不是降级（对齐 OpenClaw resolve 函数风格 `calls-config.ts:108-150`）。

---

## 8. 仓库集成与质量

- **包**：`@agent-wechat/deepseek-harness-extension`；`tsconfig` NodeNext strict（对齐 `openclaw-extension/tsconfig.json:2-17`）；esbuild ESM bundle；devDep `@deepseek-ai/dsh@0.2.2-rc.1`（类型来源，具体 peer/bundle 策略 M0 验证）；依赖 workspace `@agent-wechat/shared`。
- **AGENTS.md 义务**：用户可见功能带 changeset；每个 PR 跑并提交 `pnpm interfaces` 产物（`AGENTS.md:27-44`）。Q5 已裁决：本包纳入 `scripts/interfaces`（`index.mjs:47-50` 增列）与 release policy fixed version group（`scripts/release/policy.mjs:8`）。
- **测试**：node:test 风格对齐 openclaw-extension（97 个 TS 文件中的测试语义尽量平移）：mock Host ctx（tools/agents/storageDomain/attachments/credentials/sessionQuery/events）+ fake daemon（shared 协议回放）。安全不变量逐条有对应测试（access-before-media、24h/100、needs_review、幂等、owner 绑定、archive 默认 deny、fails-closed 语音）。
- **验证红线**：不向 live bot 发消息；端到端用隔离测试账号/容器（`AGENTS.md:24`）。
- **验证矩阵**（每条至少一个测试用例）：①契约/挂载（Config 非法组合 fail fast、output.render 可用、无 deprecated Session API）②入站恢复（重复 SSE、24h 边界、重启 newest100、cursor 提交崩溃、退避、乱序媒体）③权限（disabled/allowlist/mention、非 owner 的 archive/contact/sticker、伪造 groupId/replyTo、跨 session 工具调用全拒绝）④回复归属（并发 followup、重启补偿、只发 committed assistant 内容）⑤发送（单次 POST、原 key sendByKey、65s 上限、needs_review 不自动重发、媒体先于文本、recall 使排队回复失效）⑥文件（大小/MIME/SHA256、attachment stream abort、queued≠verified）⑦停止清理（旧 generation 迟到结果无效、SSE/下载断开、daemon stop→SIGKILL）⑧facade 安全（二期：未认证/跨 account/超限 body 拒绝，client 断连与插件 stop 都 abort）⑨登录/目录（QR 真实图片、start/wait timeout、forget 需 confirm）⑩timer/schedule 唤醒 Agent 走 `followup()` 而非 `inject()`。
- **文档**：一期仅 `README.md`（安装/配置/最小可用/验收）；Q6 已裁决不做 docs 分区。

---

## 9. 里程碑（一次架构、分阶段验收；全程保持"同等功能"为最终目标）

| 阶段 | 内容 | 验收 |
| --- | --- | --- |
| **M0 骨架** | 包结构、tsconfig/esbuild、Config schema、bundle manifest、host 冒烟（apply/服务注册/credentials 读取） | 插件经 plugin_manager 安装加载，status 工具可查 |
| **M1 native bridge** | managed/external host 生命周期、SSE committed-live-only 消费、cursor/journal、追赶、access admission | 隔离测试账号消息进入插件日志与 admission 判定（不回） |
| **M2 路由+回复** | route registry、create/resume/逐出、上下文注入、committed 回复采集→outbox→send、readPolicy、撤回 | 文本 DM（allowlist 单联系人）端到端往返；重启不丢不重 |
| **M3 当前聊天能力** | history/search/message/media 工具、媒体管线（admission→下载→转写→内容块）、recall/send_share、stickers、login | 群+DM 场景工具全通过；被拒消息媒体零下载 |
| **M4 owner+archive** | contacts/contact_request/group_info（owner 绑定）、archive 5 工具（显式授权） | owner DM 可用、群内不可见；archive 未授权时模型侧不可见 |
| **M5 通话** | calls 协议接入、SpeechProvider+v1 OpenAI、bidi、agent 模式、conferences、consultation、call-memory | 直拨双向对话；打断/超时/上限行为与 OpenClaw 文档一致；无 provider 时 unavailable 不接听 |
| **M6 收尾（可选）** | gateway facade（`webServer.register/registerUpgrade` + `connection`/`remoteAccess` 鉴权，默认关，路径语义对齐 `/plugins/agent-wechat/native`）、docs 分区、interfaces/release 纳入、changeset | 发布清单完成 |

Q2 已裁决：M0–M4 为首次交付范围（无通话，calls 默认关闭），M5 通话紧随交付；SpeechProvider 接口在 M2 期间先定稿。M0–M2 是核心链路，先行评审。

> **交付状态（实现后注记）**：M0–M5 已交付。M5 落地与原设计的差异——
> ① SpeechProvider STT/TTS 直用 PCM16/24 kHz（OpenClaw 的 mulaw/8 kHz 转换属其电话管线，不复刻）；
> ② 呼叫记忆存独立 storage domain 单元 `agent_wechat_calls`（global 单键全表，≤500 contacts×3 条）；
> ③ bidi 模式已接线 OpenAI Realtime bridge，但验收路径以 agent 咨询模式为主（daemon 无麦克风概念，`ready()` 不主动上报可用性）；
> ④ video 配置面保留但未实现；`calls.voice.video.enabled` 默认 false 且无效果。

---

## 10. 裁决记录（2026-11-23 评审通过，全部生效）

1. **Q1 语音 provider**：通过——插件自带 OpenAI 适配器（realtime + TTS + realtime STT），密钥经 `ctx.credentials` 注入。
2. **Q2 交付范围**：通过——一次架构、M0–M4 先交付（无通话），M5 通话紧随；通话模块默认关闭。
3. **Q3 会话注入语义**：通过——历史追赶用 followup 上下文注入，不伪造 seed events。
4. **Q4 授权载体**：通过——插件 Config 内按 agent 名单授权 + `when`/execute 双重校验；DSH 后续提供 preset 级授权机制再迁移。
5. **Q5 发布面**：通过——本包纳入 `scripts/interfaces` 与 release policy（fixed version group）。
6. **Q6 文档**：通过——一期仅 README；docs 分区不做。
7. **Q7 GUI 面板**：通过——一期不做 client 插件；用日志 + status 工具。

状态：**方案已批准，进入开发**（按 §9 里程碑执行，M0 开始）。

## 11. 功能对等矩阵（最终验收对照）

| 能力 | OpenClaw 机制 | 本方案 | 阶段 |
| --- | --- | --- | --- |
| DM/群接入、mention 门控、allowlist | channel+access | bridge+routing 移植 | M1–M2 |
| 24h/100 追赶、committed-only、去重 | native-monitor | 同状态机移植 | M1 |
| 媒体下载/转写/上限/pending | attachment-store+media | media 管线+SpeechProvider.stt | M3 |
| readPolicy 三态 | read pump | 同语义 | M2 |
| 撤回剔除与撤回工具 | native-monitor+tool | 同 | M2/M3 |
| 引用回复、share 卡片 | replyTo+send_share | delivery+tool | M3 |
| 贴纸 8 动作、登录 5 动作 | stickers/login tools | 同名移植 | M3 |
| 群信息、通讯录/好友请求 | owner tools | owner 绑定 | M4 |
| 全账号归档 5 工具 | archive tools | 显式授权 | M4 |
| 直拨语音 bidi/agent、会议、owner 咨询、通话记忆、20min 上限、不录音 | calls* | 协议复用+语音运行时新写 | M5 |
| gateway facade | registerHttpRoute | webServer（二期，默认关） | M6 |
| 视频通话 | 不支持 | 同样不支持（记录于配置注释） | — |
