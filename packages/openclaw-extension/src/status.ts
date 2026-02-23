export interface StatusIssue {
  channel: string;
  accountId: string;
  kind: "auth" | "runtime";
  message: string;
  fix: string;
}

export function collectWeChatStatusIssues(
  accounts: Array<{ accountId: string; connected?: boolean; linked?: boolean; lastError?: string | null; authStatus?: string }>,
): StatusIssue[] {
  const issues: StatusIssue[] = [];

  for (const snapshot of accounts) {
    if (!snapshot.connected) {
      issues.push({
        channel: "wechat",
        accountId: snapshot.accountId,
        kind: "runtime",
        message: snapshot.lastError
          ? `Cannot reach agent-wechat server: ${snapshot.lastError}`
          : "Cannot reach agent-wechat server.",
        fix: "Ensure the agent-wechat container is running (pnpm cli up)",
      });
    } else if (snapshot.authStatus === "app_not_running") {
      issues.push({
        channel: "wechat",
        accountId: snapshot.accountId,
        kind: "runtime",
        message: "WeChat application is not running. It should restart automatically.",
        fix: "If it doesn't restart, try: wx down && wx up",
      });
    } else if (snapshot.linked === false) {
      issues.push({
        channel: "wechat",
        accountId: snapshot.accountId,
        kind: "auth",
        message: "WeChat session not authenticated.",
        fix: "Run: openclaw channels login --channel wechat",
      });
    }
  }

  return issues;
}
