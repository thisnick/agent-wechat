#!/usr/bin/env node
// The "Interface approval" check. It passes when the PR leaves interfaces/ unchanged, or
// when the interfaces-approved label covers the current listings. Adding the label records
// the listings' tree hash; a later push that changes the listings removes the label.
import { execFileSync } from 'node:child_process';
import { appendFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { findComment, pullRequestEvent, removeLabel, upsertComment } from './github.mjs';
import { APPROVAL_LABEL, root } from './review.mjs';

export const APPROVAL_MARKER = '<!-- agent-wechat-interfaces-approval';

export function decide({ changed, labeled, approvedTree, currentTree, labelAdded }) {
  if (!changed) return { pass: true, action: 'none', message: 'No interface listing changed; no approval needed.' };
  if (labelAdded) return { pass: true, action: 'record', message: `Approved interface listings ${currentTree.slice(0, 12)}.` };
  if (labeled && approvedTree === currentTree) return { pass: true, action: 'none', message: `Approval covers the current listings ${currentTree.slice(0, 12)}.` };
  if (labeled) return { pass: false, action: 'reset', message: `The listings changed after approval (approved ${approvedTree?.slice(0, 12) ?? 'unknown'}, now ${currentTree.slice(0, 12)}). Review the new diff and add the ${APPROVAL_LABEL} label again.` };
  return { pass: false, action: 'none', message: `Interface listings changed. Waiting for the ${APPROVAL_LABEL} label.` };
}

const tree = ref => {
  try { return execFileSync('git', ['rev-parse', `${ref}:interfaces`], { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim(); }
  catch { return null; }
};

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const event = pullRequestEvent();
  const pr = event.pull_request;
  const baseRef = process.env.BASE_REF ?? `origin/${pr.base.ref}`;
  const mergeBase = execFileSync('git', ['merge-base', baseRef, 'HEAD'], { cwd: root, encoding: 'utf8' }).trim();
  const currentTree = tree('HEAD');
  const changed = tree(mergeBase) !== currentTree;
  const labeled = pr.labels.some(label => label.name === APPROVAL_LABEL);
  const labelAdded = event.action === 'labeled' && event.label?.name === APPROVAL_LABEL;
  const existing = changed ? await findComment(APPROVAL_MARKER, { issue: pr.number }).catch(() => undefined) : undefined;
  const approvedTree = existing?.body.match(/tree=([0-9a-f]+)/)?.[1];
  const outcome = decide({ changed, labeled, approvedTree, currentTree, labelAdded });
  const short = pr.head.sha.slice(0, 7);
  try {
    if (outcome.action === 'record') {
      await upsertComment({ issue: pr.number, marker: APPROVAL_MARKER, body: `${APPROVAL_MARKER} tree=${currentTree} -->\n**Interface listings approved** by @${event.sender?.login ?? 'unknown'} at commit \`${short}\` (listings tree \`${currentTree.slice(0, 12)}\`). A push that changes the listings removes the approval.` });
    } else if (outcome.action === 'reset') {
      await removeLabel(pr.number, APPROVAL_LABEL);
      await upsertComment({ issue: pr.number, marker: APPROVAL_MARKER, body: `${APPROVAL_MARKER} tree=none -->\n**Interface approval reset** at commit \`${short}\`. ${outcome.message}` });
    } else if (changed && !labeled && existing && !existing.body.includes('tree=none')) {
      await upsertComment({ issue: pr.number, marker: APPROVAL_MARKER, body: `${APPROVAL_MARKER} tree=none -->\n**Interface listings not approved.** The ${APPROVAL_LABEL} label was removed at commit \`${short}\`.` });
    }
  } catch (error) {
    // Fork PRs get a read-only token; the check result still enforces approval.
    console.warn(`Could not update the PR: ${error.message}`);
  }
  console.log(outcome.message);
  if (process.env.GITHUB_STEP_SUMMARY) await appendFile(process.env.GITHUB_STEP_SUMMARY, `${outcome.message}\n`);
  if (!outcome.pass) process.exitCode = 1;
}
