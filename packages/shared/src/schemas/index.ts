import { z } from "zod";

// ============================================
// SESSIONS
// ============================================

export const sessionStatusSchema = z.enum([
  "stopped",
  "starting",
  "running",
  "stopping",
  "error",
]);

export const sessionSchema = z.object({
  id: z.string(),
  name: z.string(),
  linuxUser: z.string(),
  display: z.string(),
  dbusAddress: z.string().optional(),
  vncPort: z.number().int(),
  novncPort: z.number().int(),
  status: sessionStatusSchema,
  loginState: z.lazy(() => loginStateSchema),
  loggedInUser: z.string().optional(),
  wechatPid: z.number().int().optional(),
  xvfbPid: z.number().int().optional(),
  dbusPid: z.number().int().optional(),
  errorMessage: z.string().optional(),
  createdAt: z.string(),
  updatedAt: z.string(),
});

export const createSessionParamsSchema = z.object({
  name: z.string().min(1).max(50).regex(/^[a-zA-Z0-9_-]+$/, "Name must be alphanumeric with _ or -"),
});

export const sessionIdParamsSchema = z.object({
  id: z.string().min(1),
});

export const sessionNameParamsSchema = z.object({
  name: z.string().min(1),
});

export const dbSessionRowSchema = z.object({
  id: z.string(),
  name: z.string(),
  linux_user: z.string(),
  display: z.string(),
  dbus_address: z.string().nullable(),
  vnc_port: z.number().int(),
  novnc_port: z.number().int(),
  status: z.string(),
  login_state: z.string(),
  wechat_pid: z.number().int().nullable(),
  xvfb_pid: z.number().int().nullable(),
  dbus_pid: z.number().int().nullable(),
  error_message: z.string().nullable(),
  created_at: z.string(),
  updated_at: z.string(),
});

// ============================================
// CONTAINER LIFECYCLE
// ============================================

export const upParamsSchema = z.object({
  image: z.string().optional(),
});

export const upResultSchema = z.object({
  url: z.string(),
});

export const statusSchema = z.object({
  container: z.enum(["running", "stopped", "unknown"]),
  loginState: z.discriminatedUnion("status", [
    z.object({ status: z.literal("logged_out") }),
    z.object({
      status: z.literal("qr_pending"),
      qrDataUrl: z.string().optional(),
    }),
    z.object({
      status: z.literal("logged_in"),
      userId: z.string().optional(),
    }),
  ]),
  version: z.string(),
});

// ============================================
// AUTHENTICATION
// ============================================

export const loginStateSchema = z.discriminatedUnion("status", [
  z.object({ status: z.literal("logged_out") }),
  z.object({
    status: z.literal("qr_pending"),
    qrDataUrl: z.string().optional(),
  }),
  z.object({
    status: z.literal("logged_in"),
    userId: z.string().optional(),
  }),
]);

export const loginResultSchema = z.object({
  success: z.boolean(),
  state: loginStateSchema,
});

// Login subscription events (for real-time QR monitoring)
export const loginSubscriptionEventSchema = z.discriminatedUnion("type", [
  z.object({ type: z.literal("status"), message: z.string() }),
  z.object({ type: z.literal("qr"), qrData: z.string() }),
  z.object({ type: z.literal("login_success"), userId: z.string().optional() }),
  z.object({ type: z.literal("login_timeout") }),
  z.object({ type: z.literal("error"), message: z.string() }),
]);

// ============================================
// CHATS
// ============================================

export const chatSchema = z.object({
  id: z.string(),
  username: z.string(),
  name: z.string(),
  remark: z.string().optional(),
  lastMessagePreview: z.string().optional(),
  lastMessageSender: z.string().optional(),
  lastActivityAt: z.string().optional(),
  unreadCount: z.number().int().nonnegative(),
  isGroup: z.boolean(),
});

export const listChatsParamsSchema = z.object({
  limit: z.number().int().positive().max(100).optional().default(50),
  offset: z.number().int().nonnegative().optional().default(0),
});

export const findChatParamsSchema = z.object({
  name: z.string().min(1),
});

export const getChatParamsSchema = z.object({
  id: z.string().min(1),
});

export const openChatParamsSchema = z.object({
  chatId: z.string().min(1),
});

export const openChatResultSchema = z.object({
  ok: z.boolean(),
  username: z.string().optional(),
  index: z.number().int().optional(),
  error: z.string().optional(),
});

// ============================================
// MESSAGES
// ============================================

export const messageSchema = z.object({
  localId: z.number().int(),
  serverId: z.number(),
  chatId: z.string(),
  sender: z.string().optional(),
  senderName: z.string().optional(),
  type: z.number().int(),
  content: z.string(),
  timestamp: z.string(),
});

export const listMessagesParamsSchema = z.object({
  chatId: z.string().min(1),
  limit: z.number().int().positive().max(200).optional().default(50),
  offset: z.number().int().nonnegative().optional().default(0),
});

export const sendParamsSchema = z.object({
  chatId: z.string().min(1),
  text: z.string().optional(),
  image: z.object({
    data: z.string(),
    mimeType: z.string(),
  }).optional(),
  file: z.object({
    data: z.string(),
    filename: z.string(),
  }).optional(),
});

export const sendResultSchema = z.object({
  success: z.boolean(),
  messageId: z.string().optional(),
  error: z.string().optional(),
});

export const getMediaParamsSchema = z.object({
  chatId: z.string().min(1),
  localId: z.number().int(),
});

export const mediaResultSchema = z.object({
  type: z.enum(["image", "emoji", "voice", "video", "unsupported"]),
  data: z.string().optional(),
  url: z.string().optional(),
  format: z.string(),
  filename: z.string(),
});

// ============================================
// AGENT CONFIGURATION
// ============================================

export const agentConfigSchema = z.object({
  maxTurns: z.number().int().positive().default(30),
  turnTimeout: z.number().int().positive().default(60_000),
  totalTimeout: z.number().int().positive().default(600_000),
});


// Type exports from schemas
export type UpParams = z.infer<typeof upParamsSchema>;
export type UpResult = z.infer<typeof upResultSchema>;
export type Status = z.infer<typeof statusSchema>;
export type LoginState = z.infer<typeof loginStateSchema>;
export type LoginResult = z.infer<typeof loginResultSchema>;
export type Chat = z.infer<typeof chatSchema>;
export type ListChatsParams = z.infer<typeof listChatsParamsSchema>;
export type FindChatParams = z.infer<typeof findChatParamsSchema>;
export type GetChatParams = z.infer<typeof getChatParamsSchema>;
export type OpenChatParams = z.infer<typeof openChatParamsSchema>;
export type OpenChatResult = z.infer<typeof openChatResultSchema>;
export type Message = z.infer<typeof messageSchema>;
export type ListMessagesParams = z.infer<typeof listMessagesParamsSchema>;
export type SendParams = z.infer<typeof sendParamsSchema>;
export type SendResult = z.infer<typeof sendResultSchema>;
export type GetMediaParams = z.infer<typeof getMediaParamsSchema>;
export type MediaResult = z.infer<typeof mediaResultSchema>;
export type AgentConfig = z.infer<typeof agentConfigSchema>;
export type SessionStatus = z.infer<typeof sessionStatusSchema>;
export type Session = z.infer<typeof sessionSchema>;
export type CreateSessionParams = z.infer<typeof createSessionParamsSchema>;
export type SessionIdParams = z.infer<typeof sessionIdParamsSchema>;
export type SessionNameParams = z.infer<typeof sessionNameParamsSchema>;
export type DbSessionRow = z.infer<typeof dbSessionRowSchema>;
export type LoginSubscriptionEvent = z.infer<typeof loginSubscriptionEventSchema>;
