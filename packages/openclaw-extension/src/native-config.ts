export const nativeConfigSchema = {
  type: "object", additionalProperties: false,
  required: ["mode", "accountId", "token", "consumerStateDir"],
  properties: {
    mode: { type: "string", enum: ["managed", "external"] },
    accountId: { type: "string", minLength: 1 },
    endpoint: { type: "string" }, executable: { type: "string" },
    serverConfig: { type: "string" }, dataDir: { type: "string" }, listen: { type: "string" },
    token: { anyOf: [{ type: "string", minLength: 1 }, { type: "object", additionalProperties: false,
      required: ["source", "provider", "id"], properties: {
        source: { type: "string", enum: ["env", "file", "exec", "store"] },
        provider: { type: "string", pattern: "^[a-z][a-z0-9_-]{0,63}$" },
        id: { type: "string", minLength: 1 },
      } }] }, consumerStateDir: { type: "string" },
    readPolicy: { type: "string", enum: ["never", "on_inbound", "on_model_dispatch"] },
    gatewayFacade: { type: "object", additionalProperties: false, required: ["enabled"], properties: { enabled: { type: "boolean" }, prefix: { type: "string" } } },
  },
};
export const nativePluginConfigSchema = {
  type: "object", additionalProperties: false,
  properties: { backend: { type: "string", enum: ["native"] }, native: nativeConfigSchema },
};
