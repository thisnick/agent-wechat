// Runs only from the default branch in the privileged workflow_run consumer.
import { readFile, lstat } from 'node:fs/promises';
const MARKER = '<!-- agent-wechat-interfaces -->';

export function validReport(report, run, pr, repository) {
  return Number.isSafeInteger(report.number) && report.number > 0
    && typeof report.changed === 'boolean'
    && typeof report.body === 'string' && report.body.length <= 60000
    && report.body.startsWith(MARKER)
    && run.event === 'pull_request' && pr.state === 'open'
    && pr.number === report.number && pr.base.repo.full_name === repository
    && report.head === run.head_sha && pr.head.sha === run.head_sha
    && pr.head.repo?.id === run.head_repository?.id
    && pr.head.ref === run.head_branch;
}

export async function publish({ github, context, core, path }) {
  const stat = await lstat(path);
  if (!stat.isFile() || stat.size > 260000) throw new Error('Invalid interface report file');
  const report = JSON.parse(await readFile(path, 'utf8'));
  if (!Number.isSafeInteger(report.number) || report.number <= 0) throw new Error('Invalid PR number');
  const run = context.payload.workflow_run;
  const { data: workflow } = await github.rest.actions.getWorkflow({ ...context.repo, workflow_id: run.workflow_id });
  if (workflow.path !== '.github/workflows/interfaces.yml') throw new Error('Unexpected producer workflow');
  const { data: currentRun } = await github.rest.actions.getWorkflowRun({ ...context.repo, run_id: run.id });
  if (currentRun.run_attempt !== run.run_attempt) return core.info('Skipping an older run attempt.');
  const { data: pr } = await github.rest.pulls.get({ ...context.repo, pull_number: report.number });
  if (!validReport(report, run, pr, `${context.repo.owner}/${context.repo.repo}`))
    return core.info('Skipping a stale or mismatched PR report.');
  const comments = await github.paginate(github.rest.issues.listComments, {
    ...context.repo, issue_number: pr.number, per_page: 100,
  });
  const existing = comments.find(c => c.user?.login === 'github-actions[bot]'
    && c.user?.type === 'Bot' && c.body?.startsWith(MARKER));
  // Report contents originate in PR code, so keep the source visible. They do not
  // authorize labels, merges, or any action other than updating this one comment.
  const body = `${MARKER}\nReport from PR commit \`${run.head_sha}\` ([workflow run](${run.html_url})).\n\n${report.body.slice(MARKER.length)}`;
  if (existing) await github.rest.issues.updateComment({ ...context.repo, comment_id: existing.id, body });
  else if (report.changed) await github.rest.issues.createComment({ ...context.repo, issue_number: pr.number, body });
}
