export interface StatusIssue {
  channel: string;
  accountId: string;
  kind: "auth" | "runtime";
  message: string;
  fix: string;
}

export function collectWeChatStatusIssues(
  accounts: Array<{ accountId: string; connected?: boolean; linked?: boolean; lastError?: string | null; authStatus?: string; backend?: string; handoffUnconfirmed?: boolean; needsReview?: number }>,
): StatusIssue[] {
  const issues: StatusIssue[] = [];

  for (const snapshot of accounts) {
    if (snapshot.backend === "native" && snapshot.handoffUnconfirmed) {
      issues.push({ channel: "agent-wechat", accountId: snapshot.accountId, kind: "runtime",
        message: "Prepared model handoff was not confirmed by this OpenClaw host; automatic read state remains unchanged.",
        fix: "Use a compatible prepared-run SDK hook, or select native.readPolicy=never while retaining explicit local read control." });
    }
    if (snapshot.backend === "native" && (snapshot.needsReview ?? 0) > 0) {
      issues.push({ channel: "agent-wechat", accountId: snapshot.accountId, kind: "runtime",
        message: "Native consumer work needs review after an uncertain model dispatch or reply.",
        fix: "Inspect the retained consumer receipts before retrying; do not automatically resend uncertain work." });
    }
    if (snapshot.connected === false) {
      issues.push({
        channel: "agent-wechat",
        accountId: snapshot.accountId,
        kind: "runtime",
        message: snapshot.lastError
          ? `Cannot reach agent-wechat server: ${snapshot.lastError}`
          : "Cannot reach agent-wechat server.",
        fix: snapshot.backend === "native"
          ? "Check the configured native daemon endpoint/token and owned service status (wx serve)."
          : "Ensure the agent-wechat container is running (wx-legacy up)",
      });
    } else if (snapshot.authStatus === "app_not_running") {
      issues.push({
        channel: "agent-wechat",
        accountId: snapshot.accountId,
        kind: "runtime",
        message: "WeChat application is not running. It should restart automatically.",
        fix: "If it doesn't restart, try: wx-legacy down && wx-legacy up",
      });
    } else if (snapshot.linked === false) {
      issues.push({
        channel: "agent-wechat",
        accountId: snapshot.accountId,
        kind: "auth",
        message: "WeChat session not authenticated.",
        fix: "Run: openclaw channels login --channel agent-wechat",
      });
    }
  }

  return issues;
}
