// Posts the Claude review summary as the one review comment on a pull request,
// replacing the previous one. The review workflow runs this from the base
// branch with actions/github-script; the summary text is Claude's output and is
// only ever posted as a comment body.
export const MARKER = '<!-- claude-review -->';
const NOTICE_PREFIX = '> **The Claude review of';
const MAX_SUMMARY = 60000;

/** Returns the summary from the action's structured output, or null if there is none. */
export function parseOutput(structuredOutput) {
  if (!structuredOutput) return null;
  let value;
  try {
    value = JSON.parse(structuredOutput);
  } catch {
    return null;
  }
  if (typeof value?.summary !== 'string' || !value.summary.trim()) return null;
  const findings = Number.isSafeInteger(value.findings) && value.findings >= 0 ? value.findings : null;
  return { summary: value.summary.replaceAll(MARKER, '').trim().slice(0, MAX_SUMMARY), findings };
}

function footer({ headSha, runUrl, findings }) {
  const count = findings === null ? '' : ` · ${findings} ${findings === 1 ? 'finding' : 'findings'}`;
  return `<sub>Commit \`${headSha}\` · [workflow run](${runUrl})${count}</sub>`;
}

/**
 * Builds the comment body. Without a summary the review didn't finish, so the
 * previous summary stays below a notice saying so, rather than disappearing.
 */
export function commentBody({ output, headSha, runUrl, outcome, previousBody }) {
  if (output) return `${MARKER}\n${output.summary}\n\n${footer({ headSha, runUrl, findings: output.findings })}\n`;
  const notice = `${NOTICE_PREFIX} \`${headSha.slice(0, 7)}\` did not finish** (${outcome || 'unknown'}). `
    + `See the [workflow run](${runUrl}).`;
  const kept = (previousBody ?? '')
    .slice(previousBody?.startsWith(MARKER) ? MARKER.length : 0)
    .trim()
    .split('\n')
    .filter(line => !line.startsWith(NOTICE_PREFIX))
    .join('\n')
    .trim();
  return `${MARKER}\n${notice}\n${kept ? `\n${kept}\n` : ''}`;
}

export async function publish({ github, context, core, structuredOutput, outcome, headSha, runUrl }) {
  const issue_number = context.payload.pull_request.number;
  const comments = await github.paginate(github.rest.issues.listComments, {
    ...context.repo, issue_number, per_page: 100,
  });
  const existing = comments.findLast(c => c.user?.login === 'github-actions[bot]'
    && c.user?.type === 'Bot' && c.body?.startsWith(MARKER));
  const output = parseOutput(structuredOutput);
  if (!output) core.warning(`The review returned no summary (outcome: ${outcome || 'unknown'}).`);
  const body = commentBody({ output, headSha, runUrl, outcome, previousBody: existing?.body });
  if (existing) await github.rest.issues.updateComment({ ...context.repo, comment_id: existing.id, body });
  else await github.rest.issues.createComment({ ...context.repo, issue_number, body });
}
