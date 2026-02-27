import { randomBytes } from "crypto";
import fs from "fs";
import os from "os";
import path from "path";
import { WeChatClient } from "@agent-wechat/shared";
import { resolveWeChatAccount } from "./types.js";
import { loginTerminal } from "./login.js";

const TOKEN_DIR = path.join(os.homedir(), ".config", "agent-wechat");
const TOKEN_PATH = path.join(TOKEN_DIR, "token");

function readOrGenerateToken(): string {
  try {
    const t = fs.readFileSync(TOKEN_PATH, "utf-8").trim();
    if (t) return t;
  } catch {
    // not found
  }
  fs.mkdirSync(TOKEN_DIR, { recursive: true });
  const token = randomBytes(32).toString("hex");
  fs.writeFileSync(TOKEN_PATH, token + "\n", { mode: 0o600 });
  return token;
}

export const wechatOnboardingAdapter = {
  channel: "wechat" as const,

  getStatus: async ({ cfg }: { cfg: any }) => {
    const account = resolveWeChatAccount(cfg as Record<string, unknown>);
    if (!account?.serverUrl) {
      return {
        channel: "wechat" as const,
        configured: false,
        statusLines: ["Not configured. Run: openclaw channels setup wechat"],
      };
    }

    const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });
    const statusLines: string[] = [];

    try {
      await client.status();
      statusLines.push(`Connected to ${account.serverUrl}`);
    } catch {
      return {
        channel: "wechat" as const,
        configured: true,
        statusLines: [
          `Server URL: ${account.serverUrl}`,
          "Cannot reach server — is the agent-wechat container running?",
        ],
      };
    }

    try {
      const auth = await client.authStatus();
      if (auth.status === "logged_in") {
        statusLines.push(
          `Logged in${auth.loggedInUser ? ` as ${auth.loggedInUser}` : ""}`,
        );
      } else {
        statusLines.push("Not logged in. Run: openclaw channels login --channel wechat");
      }
    } catch {
      statusLines.push("Could not check auth status");
    }

    statusLines.push(`DM policy: ${account.dmPolicy}`);
    if (account.allowFrom.length > 0) {
      statusLines.push(`Allowed senders: ${account.allowFrom.join(", ")}`);
    }
    statusLines.push(`Group policy: ${account.groupPolicy}`);

    return { channel: "wechat" as const, configured: true, statusLines };
  },

  configure: async ({ prompter, cfg }: { prompter: any; cfg: any }) => {
    const wechatCfg: Record<string, unknown> = {
      ...(cfg?.channels?.wechat ?? {}),
    };

    // 1. Server URL
    const existingUrl = (wechatCfg.serverUrl as string) ?? "http://localhost:6174";
    const serverUrl = await prompter.text({
      message: "Agent-wechat server URL",
      initialValue: existingUrl,
    });
    wechatCfg.serverUrl = serverUrl;

    // 1b. Auth token — read from file/generate if empty
    const existingToken = (wechatCfg.token as string) ?? "";
    const localDefault = existingToken || readOrGenerateToken();
    const token = await prompter.text({
      message: "Auth token (leave empty to use local token)",
      initialValue: localDefault,
    });
    wechatCfg.token = token || localDefault;

    // 2. Test connection
    const client = new WeChatClient({ baseUrl: serverUrl, token: token || undefined });
    try {
      await client.status();
    } catch {
      wechatCfg.enabled = false;
      throw new Error(
        `Cannot reach ${serverUrl}. Ensure the agent-wechat container is running.`,
      );
    }

    // 3. Check auth — offer to link during onboarding
    try {
      const auth = await client.authStatus();
      if (auth.status !== "logged_in") {
        const wantsLink = await prompter.confirm({
          message: "WeChat not logged in. Link now?",
          initialValue: true,
        });
        if (wantsLink) {
          await prompter.note(
            "Starting login — watch for QR code or phone confirmation.",
            "WeChat Login",
          );
          try {
            await loginTerminal(client, {
              onEvent: (event: any) => {
                switch (event.type) {
                  case "status":
                    prompter.note?.(event.message, "Status");
                    break;
                  case "qr":
                    prompter.note?.("Scan QR code with WeChat", "Login");
                    break;
                  case "phone_confirm":
                    prompter.note?.(
                      event.message || "Please confirm login on your phone",
                      "Confirm",
                    );
                    break;
                  case "login_success":
                    prompter.note?.("Login successful!", "Done");
                    break;
                  case "login_timeout":
                    prompter.note?.("Login timed out. Please try again.", "Timeout");
                    break;
                  case "error":
                    prompter.note?.(`Error: ${event.message}`, "Error");
                    break;
                }
              },
            });
          } catch (err) {
            prompter.note?.(
              `Login failed: ${err instanceof Error ? err.message : String(err)}`,
              "Error",
            );
          }
        } else {
          await prompter.note(
            "Run `openclaw channels login --channel wechat` later to link.",
            "WeChat",
          );
        }
      }
    } catch {
      // Auth check failed — continue setup anyway
    }

    // 4. DM policy
    const dmPolicy = await prompter.select({
      message: "DM (direct message) policy",
      options: [
        { label: "Disabled — ignore all DMs", value: "disabled" },
        { label: "Allowlist — only respond to specific senders", value: "allowlist" },
        { label: "Open — respond to all DMs", value: "open" },
      ],
    });
    wechatCfg.dmPolicy = dmPolicy;

    // 5. Allowlist (comma-separated)
    if (dmPolicy === "allowlist") {
      const raw = await prompter.text({
        message: "Allowed WeChat IDs (comma-separated wxid_xxx values)",
      });
      wechatCfg.allowFrom = raw.split(",").map((s: string) => s.trim()).filter(Boolean);
    }

    // 6. Group policy
    const groupPolicy = await prompter.select({
      message: "Group chat policy",
      options: [
        { label: "Disabled — ignore all group messages", value: "disabled" },
        { label: "Allowlist — only respond in specific groups", value: "allowlist" },
        { label: "Open — respond in all groups (when mentioned)", value: "open" },
      ],
    });
    wechatCfg.groupPolicy = groupPolicy;

    if (groupPolicy === "allowlist") {
      const raw = await prompter.text({
        message:
          "Allowed group sender IDs (comma-separated wxid_xxx values; use * to allow any sender)",
      });
      wechatCfg.groupAllowFrom = raw.split(",").map((s: string) => s.trim()).filter(Boolean);
    }

    // 7. Enable and return modified config
    wechatCfg.enabled = true;
    const newCfg = {
      ...cfg,
      channels: { ...cfg.channels, wechat: wechatCfg },
    };
    return { cfg: newCfg };
  },
};
