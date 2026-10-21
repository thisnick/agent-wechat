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
    if (snapshot.handoffUnconfirmed) {
      issues.push({ channel: "agent-wechat", accountId: snapshot.accountId, kind: "runtime",
        message: "Prepared model handoff was not confirmed by this OpenClaw host; automatic read state remains unchanged.",
        fix: "Use a compatible prepared-run SDK hook, or select native.readPolicy=never while retaining explicit local read control." });
    }
    if ((snapshot.needsReview ?? 0) > 0) {
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
        fix: "Check the configured native daemon endpoint/token and owned service status (wx serve).",
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
