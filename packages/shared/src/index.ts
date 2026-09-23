// Export types from types module
export * from "./types/index.js";

// Export HTTP client
export {
  WeChatClient,
  type WeChatClientOptions,
  type StatusResponse,
  type AuthStatus,
  type VoiceJob,
} from "./client.js";

// Export schemas (but not the inferred types which duplicate types/)
export {
  // Session schemas
  sessionStatusSchema,
  sessionSchema,
  createSessionParamsSchema,
  sessionIdParamsSchema,
  sessionNameParamsSchema,
  dbSessionRowSchema,
  // Container lifecycle schemas
  upParamsSchema,
  upResultSchema,
  statusSchema,
  // Authentication schemas
  loginStateSchema,
  loginResultSchema,
  loginSubscriptionEventSchema,
  // Chat schemas
  chatSchema,
  listChatsParamsSchema,
  findChatParamsSchema,
  getChatParamsSchema,
  openChatParamsSchema,
  openChatResultSchema,
  // Message schemas
  messageSchema,
  listMessagesParamsSchema,
  sendParamsSchema,
  sendResultSchema,
  getMediaParamsSchema,
  mediaResultSchema,
  // Agent config schema
  agentConfigSchema,
} from "./schemas/index.js";
