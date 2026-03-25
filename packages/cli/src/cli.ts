import { Command, Option } from "commander";
import { WeChatClient, type WeChatClientOptions } from "@agent-wechat/shared";
import { createSubscriptionClient, type SubscriptionClientOptions } from "./lib/client.js";
import { spawn, execSync } from "child_process";
import { randomBytes } from "crypto";
import fs from "fs";
import qrTerminal from "qrcode-terminal";
import os from "os";
import path from "path";
import { fileURLToPath } from "url";

declare const PKG_VERSION: string;
const VERSION = PKG_VERSION;
const CONTAINER_NAME = "agent-wechat";
const GHCR_IMAGE = "ghcr.io/thisnick/agent-wechat";
const DEFAULT_PORT = 6174;

// Get monorepo root (cli is at packages/cli)
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const MONOREPO_ROOT = path.resolve(__dirname, "../../..");

// Auth token paths
const TOKEN_DIR = path.join(os.homedir(), ".config", "agent-wechat");
const TOKEN_PATH = path.join(TOKEN_DIR, "token");

function ensureToken(): string {
  try {
    const existing = fs.readFileSync(TOKEN_PATH, "utf-8").trim();
    if (existing) return existing;
  } catch {
    // File doesn't exist, generate one
  }
  fs.mkdirSync(TOKEN_DIR, { recursive: true });
  const token = randomBytes(32).toString("hex");
  fs.writeFileSync(TOKEN_PATH, token + "\n", { mode: 0o600 });
  console.log(`Auth token generated: ${TOKEN_PATH}`);
  return token;
}

function printNoVncUrl() {
  const token = readToken();
  if (token) {
    console.log(`noVNC: http://localhost:${DEFAULT_PORT}/vnc/?token=${token}&autoconnect=true`);
  } else {
    console.log(`noVNC: http://localhost:${DEFAULT_PORT}/vnc/`);
  }
}

function readToken(): string | undefined {
  try {
    const t = fs.readFileSync(TOKEN_PATH, "utf-8").trim();
    return t || undefined;
  } catch {
    return undefined;
  }
}

interface Config {
  serverUrl: string;
  token?: string;
}

function getConfig(): Config {
  return {
    serverUrl: process.env.AGENT_WECHAT_URL || `http://localhost:${DEFAULT_PORT}`,
    token: process.env.AGENT_WECHAT_TOKEN || readToken(),
  };
}

function getImageTag(): string {
  return `${GHCR_IMAGE}:${VERSION}`;
}

// Create program
const program = new Command();

program
  .name("wx")
  .description("WeChat automation CLI")
  .version(VERSION)
  .option("-s, --session <name>", "Use specified session", "default");

// Helper to create REST client
function getClient(): WeChatClient {
  const config = getConfig();
  const opts = program.opts();
  return new WeChatClient({
    baseUrl: config.serverUrl,
    token: config.token,
    sessionId: opts.session,
  });
}

// Helper to get subscription client options (WebSocket login)
function getSubscriptionOptions(): SubscriptionClientOptions {
  const config = getConfig();
  const opts = program.opts();
  return {
    url: config.serverUrl,
    token: config.token,
    sessionId: opts.session,
  };
}

// ============================================
// Container Commands
// ============================================

program
  .command("up")
  .description("Start the WeChat container")
  .option("--proxy <url>", "Transparent proxy (user:pass@host:port)")
  .action((opts) => cmdUp(opts));

program
  .command("down")
  .description("Stop and remove the container")
  .action(cmdDown);

program
  .command("logs")
  .description("Show container logs")
  .action(cmdLogs);

// ============================================
// Session Commands
// ============================================

const sessionCmd = program
  .command("session")
  .description("Manage sessions");

sessionCmd
  .command("list")
  .description("List all sessions")
  .action(async () => {
    await cmdSessionList(getClient());
  });

sessionCmd
  .command("create <name>")
  .description("Create a new session")
  .action(async (name: string) => {
    await cmdSessionCreate(getClient(), name);
  });

sessionCmd
  .command("start <id>")
  .description("Start a session")
  .action(async (id: string) => {
    await cmdSessionStart(getClient(), id);
  });

sessionCmd
  .command("stop <id>")
  .description("Stop a session")
  .action(async (id: string) => {
    await cmdSessionStop(getClient(), id);
  });

sessionCmd
  .command("delete <id>")
  .description("Delete a session")
  .action(async (id: string) => {
    await cmdSessionDelete(getClient(), id);
  });

// ============================================
// API Commands
// ============================================

program
  .command("status")
  .description("Show container and login status")
  .action(async () => {
    await cmdStatus(getClient());
  });

// ============================================
// Auth Commands
// ============================================

const authCmd = program
  .command("auth")
  .description("Authentication commands");

authCmd
  .command("login")
  .description("Log in to WeChat (shows QR code)")
  .option("-t, --timeout <seconds>", "Timeout in seconds", "300")
  .option("-n, --new", "Switch to new account instead of existing")
  .action(async (opts) => {
    const timeoutMs = parseInt(opts.timeout, 10) * 1000;
    await cmdLogin(getSubscriptionOptions(), timeoutMs, opts.new ?? false);
  });

authCmd
  .command("logout")
  .description("Log out of WeChat")
  .action(async () => {
    const client = getClient();
    const result = await client.logout();
    if (result.success) {
      console.log("Logged out");
    } else {
      console.log(`Logout failed: ${result.error ?? "unknown error"}`);
    }
  });

authCmd
  .command("status")
  .description("Check login status")
  .action(async () => {
    const client = getClient();
    const auth = await client.authStatus();
    if (auth.status === "logged_in") {
      console.log(`Logged in${auth.loggedInUser ? ` as ${auth.loggedInUser}` : ""}`);
    } else {
      console.log(`Status: ${auth.status.replace(/_/g, " ")}`);
    }
  });

authCmd
  .command("token")
  .description("Show or regenerate the auth token")
  .option("--regenerate", "Generate a new token")
  .action(async (opts: { regenerate?: boolean }) => {
    if (opts.regenerate) {
      fs.mkdirSync(TOKEN_DIR, { recursive: true });
      const token = randomBytes(32).toString("hex");
      fs.writeFileSync(TOKEN_PATH, token + "\n", { mode: 0o600 });
      console.log(`New token: ${token}`);
      console.log(`Restart the container for it to take effect: pnpm cli down && pnpm cli up`);
    } else {
      const token = ensureToken();
      console.log(token);
    }
  });

// ============================================
// Chats Commands
// ============================================

const chatsCmd = program
  .command("chats")
  .description("Chat management commands");

chatsCmd
  .command("list")
  .description("List chats from WeChat database")
  .option("-l, --limit <number>", "Maximum number of chats", "50")
  .option("-o, --offset <number>", "Skip first N chats", "0")
  .option("-j, --json", "Output as JSON")
  .action(async (opts) => {
    await cmdChats(getClient(), parseInt(opts.limit, 10), parseInt(opts.offset, 10), opts.json ?? false);
  });

chatsCmd
  .command("get <chatId>")
  .description("Get details for a specific chat")
  .option("-j, --json", "Output as JSON")
  .action(async (chatId: string, opts) => {
    await cmdChatGet(getClient(), chatId, opts.json ?? false);
  });

chatsCmd
  .command("find <name>")
  .description("Find chat by name")
  .action(async (name: string) => {
    await cmdFind(getClient(), name);
  });

chatsCmd
  .command("open <chatId>")
  .description("Open a chat in WeChat UI (triggers media downloads + clears unread)")
  .option("--clear-unreads", "Clear unread count after opening")
  .action(async (chatId: string, opts: { clearUnreads?: boolean }) => {
    await cmdChatOpen(getClient(), chatId, opts.clearUnreads);
  });

// ============================================
// Contacts Commands
// ============================================

const contactsCmd = program
  .command("contacts")
  .description("Contact management commands");

contactsCmd
  .command("list")
  .description("List all contacts from WeChat address book")
  .option("-l, --limit <number>", "Maximum number of contacts", "200")
  .option("-o, --offset <number>", "Skip first N contacts", "0")
  .option("-j, --json", "Output as JSON")
  .action(async (opts) => {
    await cmdContacts(getClient(), parseInt(opts.limit, 10), parseInt(opts.offset, 10), opts.json ?? false);
  });

contactsCmd
  .command("find <name>")
  .description("Search contacts by name")
  .option("-j, --json", "Output as JSON")
  .action(async (name: string, opts) => {
    await cmdContactsFind(getClient(), name, opts.json ?? false);
  });

// ============================================
// Messages Commands
// ============================================

const messagesCmd = program
  .command("messages")
  .description("Message commands");

messagesCmd
  .command("list <chatId>")
  .description("List messages for a chat")
  .option("-l, --limit <number>", "Maximum number of messages", "50")
  .option("-o, --offset <number>", "Skip first N messages", "0")
  .option("-j, --json", "Output as JSON")
  .action(async (chatId: string, opts) => {
    await cmdMessages(getClient(), chatId, parseInt(opts.limit, 10), parseInt(opts.offset, 10), opts.json ?? false);
  });

messagesCmd
  .command("media <chatId> <localId>")
  .description("Save media attachment (image thumbnail, emoji, or voice)")
  .option("-o, --output <path>", "Output file path")
  .action(async (chatId: string, localIdStr: string, opts) => {
    await cmdMedia(getClient(), chatId, parseInt(localIdStr, 10), opts.output);
  });

messagesCmd
  .command("send <chatId>")
  .description("Send a message to a chat")
  .option("--text <text>", "Text message to send")
  .option("--image <path>", "Image file to send")
  .option("--file <path>", "File to send")
  .action(async (chatId: string, opts: { text?: string; image?: string; file?: string }) => {
    if (!opts.text && !opts.image && !opts.file) {
      console.error("Must provide --text, --image, or --file");
      process.exit(1);
    }

    let image: { data: string; mimeType: string } | undefined;
    if (opts.image) {
      if (!fs.existsSync(opts.image)) {
        console.error(`File not found: ${opts.image}`);
        process.exit(1);
      }
      const data = fs.readFileSync(opts.image);
      const ext = path.extname(opts.image).toLowerCase();
      const mimeType = ext === ".jpg" || ext === ".jpeg" ? "image/jpeg" :
                       ext === ".gif" ? "image/gif" : "image/png";
      image = { data: data.toString("base64"), mimeType };
    }

    let file: { data: string; filename: string } | undefined;
    if (opts.file) {
      if (!fs.existsSync(opts.file)) {
        console.error(`File not found: ${opts.file}`);
        process.exit(1);
      }
      const data = fs.readFileSync(opts.file);
      file = { data: data.toString("base64"), filename: path.basename(opts.file) };
    }

    await cmdSend(getClient(), chatId, opts.text, image, file);
  });

// ============================================
// Update Command
// ============================================

program
  .command("update")
  .description("Update the agent-server binary in the running container to match CLI version")
  .action(cmdUpdate);

// ============================================
// Debug Commands
// ============================================

program
  .command("screenshot")
  .description("Save screenshot to file")
  .argument("[file]", "Output file path", "screenshot.png")
  .action(async (file: string) => {
    await cmdScreenshot(getClient(), file);
  });

program
  .command("a11y")
  .description("Dump accessibility tree")
  .addOption(
    new Option("-f, --format <format>", "Output format")
      .choices(["json", "aria"])
      .default("json")
  )
  .action(async (options: { format: "json" | "aria" }) => {
    await cmdA11y(getClient(), options.format);
  });

// ============================================
// Command Implementations
// ============================================

async function cmdStatus(client: WeChatClient) {
  const container = getContainerRuntimeState();
  if (container === "up") {
    console.log("Container: up");
  } else if (container === "down") {
    console.log("Container: down");
    return;
  } else {
    console.log("Container: unknown (Docker unavailable)");
    return;
  }

  try {
    const status = await client.status();
    console.log("Server: reachable");
    console.log("Version:", status.version);
  } catch (err) {
    console.log("Server: unreachable");
    console.log(`Error: ${err instanceof Error ? err.message : String(err)}`);
    return;
  }

  try {
    const auth = await client.authStatus();
    if (auth.status === "logged_in") {
      console.log(`Login: logged in${auth.loggedInUser ? ` as ${auth.loggedInUser}` : ""}`);
    } else {
      console.log(`Login: ${auth.status.replace(/_/g, " ")}`);
    }
  } catch {
    console.log("Login: unknown (auth status unavailable)");
  }
}

function getContainerRuntimeState(): "up" | "down" | "unknown" {
  try {
    const existing = execSync(`docker ps -aq -f "name=^${CONTAINER_NAME}$"`, {
      encoding: "utf-8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
    if (!existing) {
      return "down";
    }
    const running = execSync(`docker ps -q -f "name=^${CONTAINER_NAME}$"`, {
      encoding: "utf-8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
    return running ? "up" : "down";
  } catch {
    return "unknown";
  }
}

async function cmdLogin(
  options: SubscriptionClientOptions,
  timeoutMs: number = 300_000,
  newAccount: boolean = false,
) {
  console.log(newAccount ? "Initiating login with new account...\n" : "Initiating login...\n");

  const { client, close } = createSubscriptionClient(options);
  let subscription: { unsubscribe: () => void } | null = null;

  // Handle Ctrl+C to abort subscription
  const abortHandler = () => {
    console.log("\n\nLogin cancelled.");
    if (subscription) {
      subscription.unsubscribe();
    }
    close();
    process.exit(0);
  };
  process.on("SIGINT", abortHandler);

  try {
    await new Promise<void>((resolve, reject) => {
      subscription = client.status.loginSubscription.subscribe(
        {
          timeoutMs,
          newAccount,
        },
        {
          onData: (event) => {
            switch (event.type) {
              case "status":
                console.log(`Status: ${event.message}`);
                break;
              case "qr":
                console.log("Scan this QR code with WeChat:\n");
                // Use binaryData if available (preserves exact bytes), fallback to string
                const qrInput = event.qrBinaryData
                  ? Buffer.from(event.qrBinaryData as number[]).toString("utf-8")
                  : event.qrData;
                qrTerminal.generate(qrInput as string, { small: true });
                console.log("\nWaiting for scan... (Ctrl+C to cancel)\n");
                break;
              case "phone_confirm":
                console.log(`\n📱 ${event.message || "Please confirm login on your phone"}\n`);
                break;
              case "login_success":
                console.log("\n\nLogin successful!");
                if (event.userId) {
                  console.log(`User ID: ${event.userId}`);
                }
                resolve();
                break;
              case "login_timeout":
                console.log("\n\nLogin timed out. Please try again.");
                resolve();
                break;
              case "error":
                console.error(`\nError: ${event.message}`);
                reject(new Error(event.message));
                break;
            }
          },
          onError: (err) => {
            console.error("\nConnection error:", err.message);
            reject(err);
          },
          onComplete: () => {
            // Subscription completed normally
          },
        }
      );
    });
  } finally {
    process.removeListener("SIGINT", abortHandler);
    close();
  }
}

async function cmdChats(client: WeChatClient, limit: number = 50, offset: number = 0, json: boolean = false) {
  const chats = await client.listChats(limit, offset);

  if (json) {
    console.log(JSON.stringify(chats, null, 2));
    return;
  }

  if (chats.length === 0) {
    console.log("No chats found. Make sure you're logged in.");
    return;
  }

  console.log(`Found ${chats.length} chat(s):\n`);

  // Chat ID column width based on actual data
  const maxIdLen = Math.max(10, ...chats.map(c => c.username?.length ?? c.id.length));
  const idHeader = "Chat ID".padEnd(maxIdLen);
  console.log(`${idHeader}  Unread  Group  Name`);
  console.log("-".repeat(maxIdLen + 30));
  for (const chat of chats) {
    const id = (chat.username ?? chat.id).padEnd(maxIdLen);
    const unread = chat.unreadCount > 0 ? String(chat.unreadCount).padStart(2) : "  ";
    const group = chat.isGroup ? "  Y  " : "     ";
    console.log(`${id}  ${unread}    ${group}  ${chat.name}`);
  }
}

async function cmdChatGet(client: WeChatClient, chatId: string, json: boolean = false) {
  const chat = await client.getChat(chatId);

  if (!chat) {
    console.error(`Chat not found: ${chatId}`);
    process.exit(1);
  }

  if (json) {
    console.log(JSON.stringify(chat, null, 2));
    return;
  }

  console.log(`Chat ID:        ${chat.username ?? chat.id}`);
  console.log(`Name:           ${chat.name}`);
  if (chat.remark) console.log(`Remark:         ${chat.remark}`);
  console.log(`Group:          ${chat.isGroup ? "Yes" : "No"}`);
  console.log(`Unread:         ${chat.unreadCount}`);
  if (chat.lastMessagePreview) {
    const sender = chat.lastMessageSender ? `${chat.lastMessageSender}: ` : "";
    console.log(`Last message:   ${sender}${chat.lastMessagePreview}`);
  }
  if (chat.lastActivityAt) console.log(`Last activity:  ${chat.lastActivityAt}`);
}

/** WeChat base message types */
const MSG_BASE_TYPES: Record<number, string> = {
  1: "text",
  3: "image",
  34: "voice",
  43: "video",
  47: "emoji",
  49: "appmsg",
  10000: "system",
  10002: "revoke",
};

/** Appmsg (type 49) subtypes */
const APPMSG_SUB_TYPES: Record<number, string> = {
  1: "text-link",
  3: "music",
  4: "video",
  5: "link",
  6: "file",
  8: "sticker",
  19: "location",
  33: "mini-program",
  36: "mini-program",
  57: "reply",
  63: "livestream",
};

function getMsgTypeLabel(rawType: number): string {
  const base = rawType & 0xFFFFFFFF;
  const sub = Math.floor(rawType / 0x100000000);

  const baseLabel = MSG_BASE_TYPES[base];
  if (!baseLabel) return `type:${rawType}`;

  if (base === 49 && sub > 0) {
    return APPMSG_SUB_TYPES[sub] ?? `appmsg:${sub}`;
  }
  return baseLabel;
}

async function cmdMessages(client: WeChatClient, chatId: string, limit: number = 50, offset: number = 0, json: boolean = false) {
  const messages = await client.listMessages(chatId, limit, offset);

  if (json) {
    console.log(JSON.stringify(messages, null, 2));
    return;
  }

  if (messages.length === 0) {
    console.log("No messages found.");
    return;
  }

  // Display messages oldest-first for natural reading order
  const sorted = [...messages].reverse();

  // Compute column widths
  const maxIdLen = Math.max(2, ...sorted.map(m => String(m.localId).length));
  const maxTypeLen = Math.max(4, ...sorted.map(m => getMsgTypeLabel(m.type).length));
  const formatSender = (m: (typeof sorted)[number]) => {
    const name = m.senderName;
    const id = m.sender;
    if (name && id) return `${name}(${id.slice(0, 10)})`;
    if (name) return name;
    if (id) return id.slice(0, 10);
    return "";
  };
  const maxSenderLen = Math.max(6, ...sorted.map(m => formatSender(m).length));
  const hasAnyMention = sorted.some(m => m.isMentioned);
  const atCol = hasAnyMention ? "@me  " : "";
  const atHeader = hasAnyMention ? "@me".padEnd(5) : "";

  // Header
  console.log(`${"ID".padEnd(maxIdLen)}  ${"Time".padEnd(22)}  ${atHeader}${"Type".padEnd(maxTypeLen)}  ${"Sender".padEnd(maxSenderLen)}  Message`);
  console.log("-".repeat(maxIdLen + 22 + maxTypeLen + maxSenderLen + (hasAnyMention ? 5 : 0) + 10));

  for (const msg of sorted) {
    const time = new Date(msg.timestamp).toLocaleString();
    const typeLabel = getMsgTypeLabel(msg.type);
    const id = String(msg.localId).padEnd(maxIdLen);
    const mention = hasAnyMention ? (msg.isMentioned ? "Y" : "").padEnd(5) : "";
    const sender = formatSender(msg).padEnd(maxSenderLen);
    let preview = msg.content.length > 120 ? msg.content.slice(0, 120) + "..." : msg.content;
    if (msg.reply) {
      const rSender = msg.reply.sender ? `${msg.reply.sender}: ` : "";
      const rSnippet = msg.reply.content.length > 40 ? msg.reply.content.slice(0, 40) + "..." : msg.reply.content;
      preview = `[Re: ${rSender}${rSnippet}] ${preview}`;
    }

    console.log(`${id}  ${time.padEnd(22)}  ${mention}${typeLabel.padEnd(maxTypeLen)}  ${sender}  ${preview}`);
  }

  console.log(`\n${messages.length} message(s) shown.`);
}

async function cmdMedia(client: WeChatClient, chatId: string, localId: number, outputPath?: string) {
  const result = await client.getMedia(chatId, localId);

  if (result.type === "unsupported") {
    console.error("No media found for this message (unsupported type or not found).");
    process.exit(1);
  }
  if (result.type === "pending") {
    console.error("Media not yet available. WeChat may still be downloading it — try again shortly.");
    process.exit(1);
  }

  const outFile = outputPath ?? result.filename;

  if (result.data) {
    // Decode base64
    const buffer = Buffer.from(result.data, "base64");
    fs.writeFileSync(outFile, buffer);
    console.log(`Saved ${result.type} to ${outFile} (${buffer.length} bytes)`);
  } else if (result.type === "image") {
    console.error("Image thumbnail not yet cached by WeChat. Try opening the chat in the app first.");
    process.exit(1);
  } else if (result.type === "video") {
    console.error("Video not yet downloaded by WeChat. Try playing the video in the app first.");
    process.exit(1);
  } else {
    console.error(`Media type "${result.type}" has no downloadable data.`);
    process.exit(1);
  }
}

async function cmdFind(client: WeChatClient, name: string) {
  const chats = await client.findChats(name);
  if (chats.length === 0) {
    console.log(`No chats found matching "${name}"`);
    return;
  }

  console.log(`Found ${chats.length} matching chats:\n`);
  for (const chat of chats) {
    console.log(`  ${chat.id}: ${chat.name}`);
  }
}

async function cmdChatOpen(client: WeChatClient, chatId: string, clearUnreads?: boolean) {
  console.log(`Opening chat ${chatId}...`);
  const result = await client.openChat(chatId, clearUnreads);

  if (result.ok) {
    console.log(`Chat opened: ${result.username} (index ${result.index})`);
  } else if (result.error === "NOT_LOGGED_IN") {
    console.error("Not logged in. Run: pnpm cli auth login");
    process.exit(1);
  } else {
    console.error(`Failed: ${result.error}`);
    process.exit(1);
  }
}

async function cmdContacts(client: WeChatClient, limit: number = 200, offset: number = 0, json: boolean = false) {
  const contacts = await client.listContacts(limit, offset);

  if (json) {
    console.log(JSON.stringify(contacts, null, 2));
    return;
  }

  if (contacts.length === 0) {
    console.log("No contacts found. Make sure you're logged in.");
    return;
  }

  console.log(`Found ${contacts.length} contact(s):\n`);

  const maxIdLen = Math.max(10, ...contacts.map(c => c.username.length));
  const idHeader = "Username".padEnd(maxIdLen);
  console.log(`${idHeader}  Type        Name`);
  console.log("-".repeat(maxIdLen + 30));
  for (const c of contacts) {
    const id = c.username.padEnd(maxIdLen);
    const type = c.contactType.padEnd(10);
    const name = c.remark ? `${c.nickName} (${c.remark})` : c.nickName;
    console.log(`${id}  ${type}  ${name}`);
  }
}

async function cmdContactsFind(client: WeChatClient, name: string, json: boolean = false) {
  const contacts = await client.findContacts(name);

  if (json) {
    console.log(JSON.stringify(contacts, null, 2));
    return;
  }

  if (contacts.length === 0) {
    console.log(`No contacts found matching "${name}"`);
    return;
  }

  console.log(`Found ${contacts.length} matching contact(s):\n`);
  for (const c of contacts) {
    const name = c.remark ? `${c.nickName} (${c.remark})` : c.nickName;
    console.log(`  ${c.username}: ${name} [${c.contactType}]`);
  }
}

async function cmdSend(client: WeChatClient, chatId: string, text?: string, image?: { data: string; mimeType: string }, file?: { data: string; filename: string }) {
  const what = file ? `file "${file.filename}"` : image ? "image" : "message";
  console.log(`Sending ${what} to ${chatId}...`);
  const result = await client.sendMessage({
    chatId,
    ...(text ? { text } : {}),
    ...(image ? { image } : {}),
    ...(file ? { file } : {}),
  });

  if (result.success) {
    console.log("Message sent successfully!");
    if (result.messageId) {
      console.log(`Message ID: ${result.messageId}`);
    }
  } else if (result.error === "NOT_LOGGED_IN") {
    console.error("Not logged in. Run: pnpm cli auth login");
    process.exit(1);
  } else {
    console.error(`Failed to send message: ${result.error || "Unknown error"}`);
    process.exit(1);
  }
}

async function cmdScreenshot(client: WeChatClient, outputPath: string) {
  console.log(`Capturing screenshot...`);
  const result = await client.screenshot();
  const buffer = Buffer.from(result.base64, "base64");
  fs.writeFileSync(outputPath, buffer);
  console.log(`Screenshot saved to ${outputPath}`);
}

async function cmdA11y(client: WeChatClient, format: "json" | "aria") {
  const result = await client.a11y(format);
  if (result.error) {
    console.error(`Error: ${result.error}`);
    return;
  }
  if (format === "aria" && result.aria) {
    console.log(result.aria);
  } else if (result.tree) {
    console.log(JSON.stringify(result.tree, null, 2));
  }
}

// ============================================
// Session Commands Implementation
// ============================================

async function cmdSessionList(client: WeChatClient) {
  const sessions = await client.listSessions();
  if (sessions.length === 0) {
    console.log("No sessions found.");
    return;
  }

  console.log(`Found ${sessions.length} session(s):\n`);
  for (const session of sessions) {
    const status = session.status === "running" ? "✓ running" : session.status;
    const login = session.loginState.status === "logged_in" ? "logged in" : session.loginState.status;
    console.log(`  ${session.id}: ${session.name}`);
    console.log(`    Status: ${status}, Login: ${login}`);
    console.log(`    Display: ${session.display}, VNC: ${session.vncPort}`);
    console.log(`    User: ${session.linuxUser}`);
    if (session.errorMessage) {
      console.log(`    Error: ${session.errorMessage}`);
    }
    console.log();
  }
}

async function cmdSessionCreate(client: WeChatClient, name: string) {
  console.log(`Creating session "${name}"...`);
  const session = await client.createSession(name);
  console.log(`Session created!`);
  console.log(`  ID: ${session.id}`);
  console.log(`  Name: ${session.name}`);
  console.log(`  User: ${session.linuxUser}`);
  console.log(`  Display: ${session.display}`);
  console.log(`  VNC Port: ${session.vncPort}`);
  console.log(`\nStart the session with: pnpm cli session start ${session.name}`);
}

async function cmdSessionStart(client: WeChatClient, idOrName: string) {
  console.log(`Starting session "${idOrName}"...`);
  const session = await client.startSession(idOrName);
  console.log(`Session started!`);
  console.log(`  Status: ${session.status}`);
  console.log(`  Display: ${session.display}`);
  console.log(`  VNC Port: ${session.vncPort}`);
  if (session.dbusAddress) {
    console.log(`  D-Bus: ${session.dbusAddress}`);
  }
  console.log(`\nLogin with: pnpm cli --session ${session.name} login`);
}

async function cmdSessionStop(client: WeChatClient, idOrName: string) {
  console.log(`Stopping session "${idOrName}"...`);
  const session = await client.stopSession(idOrName);
  console.log(`Session stopped.`);
  console.log(`  Status: ${session.status}`);
}

async function cmdSessionDelete(client: WeChatClient, idOrName: string) {
  console.log(`Deleting session "${idOrName}"...`);
  const result = await client.deleteSession(idOrName);
  if (result.success) {
    console.log(`Session deleted.`);
  } else {
    console.error(`Failed to delete session.`);
    process.exit(1);
  }
}

// ============================================
// Update Command Implementation
// ============================================

async function cmdUpdate() {
  const version = VERSION;
  console.log(`Updating agent-server to v${version}...`);

  // Find running container
  let container: string;
  try {
    container = execSync(
      `docker ps --filter "name=agent-wechat" --format "{{.Names}}" | head -1`,
      { encoding: "utf-8" }
    ).trim();
  } catch {
    container = "";
  }
  if (!container) {
    console.error("No running agent-wechat container found.");
    process.exit(1);
  }

  // Detect container architecture
  const uname = execSync(
    `docker exec "${container}" uname -m`,
    { encoding: "utf-8" }
  ).trim();
  const arch = uname === "x86_64" ? "amd64" : "arm64";

  const assetName = `agent-server-${version}-linux-${arch}`;
  const tmpFile = path.join(os.tmpdir(), assetName);

  // Download binary from GitHub Releases (no gh CLI dependency)
  const releaseUrl = `https://github.com/thisnick/agent-wechat/releases/download/v${version}/${assetName}`;
  console.log(`Downloading ${assetName}...`);
  try {
    const resp = await fetch(releaseUrl, { redirect: "follow" });
    if (!resp.ok) {
      throw new Error(`HTTP ${resp.status}: ${resp.statusText}`);
    }
    const buffer = Buffer.from(await resp.arrayBuffer());
    fs.writeFileSync(tmpFile, buffer, { mode: 0o755 });
  } catch (err) {
    console.error(
      `Failed to download ${assetName} from GitHub Releases.\n` +
      `Make sure v${version} has been released with binary assets.\n` +
      `URL: ${releaseUrl}\n` +
      `Error: ${err instanceof Error ? err.message : String(err)}`
    );
    process.exit(1);
  }

  // Deploy into container
  console.log(`Deploying to ${container}...`);
  execSync(`docker cp "${tmpFile}" "${container}:/opt/agent-server/agent-server"`, {
    stdio: "inherit",
  });
  execSync(`docker exec "${container}" chmod +x /opt/agent-server/agent-server`, {
    stdio: "inherit",
  });

  // Restart server process (entrypoint loop brings it back)
  execSync(
    `docker exec "${container}" pkill -f "/opt/agent-server/agent-server" 2>/dev/null || true`,
    { stdio: "inherit" }
  );

  // Clean up
  try {
    fs.unlinkSync(tmpFile);
  } catch {
    // ignore
  }

  console.log("Server restarting with new binary.");

  // Wait for health check
  console.log("Waiting for server...");
  const config = getConfig();
  for (let i = 0; i < 15; i++) {
    await new Promise((r) => setTimeout(r, 1000));
    try {
      const resp = await fetch(`${config.serverUrl}/health`);
      if (resp.ok) {
        console.log("Server is ready!");
        return;
      }
    } catch {
      // not ready yet
    }
  }
  console.log("Server did not become ready in time. Check logs with: wx logs");
}

// ============================================
// Container Commands Implementation
// ============================================

async function cmdUp(opts: { proxy?: string } = {}) {
  let image = getImageTag();

  // Check if container already exists
  try {
    const existingId = execSync(`docker ps -aq -f "name=^${CONTAINER_NAME}$"`, { encoding: "utf-8" }).trim();
    if (existingId) {
      const running = execSync(`docker ps -q -f "name=^${CONTAINER_NAME}$"`, { encoding: "utf-8" }).trim();
      if (running) {
        console.log(`Container ${CONTAINER_NAME} is already running.`);
        console.log(`API: http://localhost:${DEFAULT_PORT}`);
        printNoVncUrl();
        return;
      }
      console.log(`Starting existing container ${CONTAINER_NAME}...`);
      execSync(`docker start ${CONTAINER_NAME}`, { stdio: "inherit" });
      console.log(`API: http://localhost:${DEFAULT_PORT}`);
      printNoVncUrl();
      return;
    }
  } catch {
    // No container found, continue to create
  }

  // Check if image exists locally, pull if not
  try {
    execSync(`docker image inspect ${image}`, { stdio: "ignore" });
  } catch {
    console.log(`Image ${image} not found locally. Pulling...`);
    try {
      execSync(`docker pull ${image}`, { stdio: "inherit" });
    } catch {
      // Versioned tag may not exist yet — fall back to latest
      const fallback = `${GHCR_IMAGE}:latest`;
      if (image !== fallback) {
        console.log(`Tag ${VERSION} not found, trying latest...`);
        try {
          execSync(`docker pull ${fallback}`, { stdio: "inherit" });
          image = fallback;
        } catch {
          console.error(`Failed to pull ${fallback}. Check your internet connection and Docker setup.`);
          process.exit(1);
        }
      } else {
        console.error(`Failed to pull ${image}. Check your internet connection and Docker setup.`);
        process.exit(1);
      }
    }
  }

  // Ensure auth token exists
  const token = ensureToken();

  console.log(`Starting container ${CONTAINER_NAME} from ${image}...`);

  const dockerArgs = [
    "run", "-d",
    "--name", CONTAINER_NAME,
    "--security-opt", "seccomp=unconfined",
    "--cap-add=SYS_PTRACE",
    "--cap-add=NET_ADMIN",
    "-p", `${DEFAULT_PORT}:${DEFAULT_PORT}`,
    "-v", `${CONTAINER_NAME}-data:/data`,
    "-v", `${CONTAINER_NAME}-wechat-home:/home/wechat`,
    "-v", `${TOKEN_PATH}:/data/auth-token:ro`,
  ];

  if (opts.proxy) {
    dockerArgs.push("-e", `PROXY=${opts.proxy}`);
  }

  dockerArgs.push(image);

  try {
    execSync(`docker ${dockerArgs.join(" ")}`, { stdio: "inherit" });
    console.log(`\nContainer started successfully!`);
    console.log(`API: http://localhost:${DEFAULT_PORT}`);
    printNoVncUrl();
    console.log(`\nWaiting for server to be ready...`);

    for (let i = 0; i < 30; i++) {
      try {
        const response = await fetch(`http://localhost:${DEFAULT_PORT}/health`);
        if (response.ok) {
          console.log("Server is ready!");
          return;
        }
      } catch {
        // Not ready yet
      }
      await new Promise(r => setTimeout(r, 1000));
      process.stdout.write(".");
    }
    console.log("\nServer did not become ready in time. Check logs with: wx logs");
  } catch (error) {
    console.error("Failed to start container:", error);
    process.exit(1);
  }
}

async function cmdDown() {
  console.log(`Stopping container ${CONTAINER_NAME}...`);
  try {
    execSync(`docker stop ${CONTAINER_NAME}`, { stdio: "inherit" });
    execSync(`docker rm ${CONTAINER_NAME}`, { stdio: "inherit" });
    console.log("Container stopped and removed.");
  } catch {
    console.log("Container not found or already stopped.");
  }
}

async function cmdLogs() {
  try {
    const logs = spawn("docker", ["logs", "-f", CONTAINER_NAME], {
      stdio: "inherit",
    });
    logs.on("error", () => {
      console.error(`Container ${CONTAINER_NAME} not found.`);
      process.exit(1);
    });
  } catch {
    console.error(`Container ${CONTAINER_NAME} not found.`);
    process.exit(1);
  }
}

// Parse and run
program.parseAsync(process.argv).catch((err) => {
  console.error("Fatal error:", err);
  process.exit(1);
});
