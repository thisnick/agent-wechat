// Posts the Claude review summary as the one review comment on a pull request,
// replacing the previous one. The review workflow runs this from the base
// branch with actions/github-script; the summary text is Claude's output and is
// only ever posted as a comment body.
export const MARKER = '<!-- claude-review -->';
const NOTICE_PREFIX = '> **The Claude review of';
const MAX_SUMMARY = 60000;

const count = value => (Number.isSafeInteger(value) && value >= 0 ? value : null);

/** Returns the review result from the action's structured output, or null if there is none. */
export function parseOutput(structuredOutput) {
  if (!structuredOutput) return null;
  let value;
  try {
    value = JSON.parse(structuredOutput);
  } catch {
    return null;
  }
  if (typeof value?.summary !== 'string' || !value.summary.trim()) return null;
  const confidence = Number.isSafeInteger(value.confidence) && value.confidence >= 1 && value.confidence <= 10
    ? value.confidence : null;
  return {
    summary: value.summary.replaceAll(MARKER, '').trim().slice(0, MAX_SUMMARY),
    confidence,
    important: count(value.important),
    nits: count(value.nits),
    preexisting: count(value.preexisting),
  };
}

/** The line under the heading: the confidence score, then the findings by severity. */
export function scoreLine(output) {
  const parts = output.confidence === null ? [] : [`**Confidence: ${output.confidence}/10**`];
  const counts = [
    [output.important, 'important', 'important'],
    [output.nits, 'nit', 'nits'],
    [output.preexisting, 'pre-existing', 'pre-existing'],
  ];
  if (counts.every(([n]) => n === 0)) parts.push('Nothing to fix');
  else for (const [n, one, many] of counts) if (n) parts.push(`${n} ${n === 1 ? one : many}`);
  return parts.join(' · ');
}

/**
 * Builds the comment body. Without a summary the review didn't finish, so the
 * previous summary stays below a notice saying so, rather than disappearing.
 */
export function commentBody({ output, headSha, runUrl, outcome, previousBody }) {
  if (output) {
    const score = scoreLine(output);
    return `${MARKER}\n## Claude review of \`${headSha.slice(0, 7)}\`\n\n${score ? `${score}\n\n` : ''}`
      + `${output.summary}\n\n<sub>Commit \`${headSha}\` · [workflow run](${runUrl})</sub>\n`;
  }
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
