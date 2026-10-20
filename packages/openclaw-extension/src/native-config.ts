export const nativeConfigSchema = {
  type: "object", additionalProperties: false,
  required: ["mode", "accountId", "tokenFile", "consumerStateDir"],
  properties: {
    mode: { type: "string", enum: ["managed", "external"] },
    accountId: { type: "string", minLength: 1 },
    endpoint: { type: "string" }, executable: { type: "string" },
    serverConfig: { type: "string" }, dataDir: { type: "string" }, listen: { type: "string" },
    tokenFile: { type: "string" }, consumerStateDir: { type: "string" },
    readPolicy: { type: "string", enum: ["never", "on_model_dispatch"] },
  },
};
export const nativePluginConfigSchema = {
  type: "object", additionalProperties: false,
  properties: { backend: { type: "string", enum: ["legacy", "native"] }, native: nativeConfigSchema },
};
