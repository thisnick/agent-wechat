/**
 * Pure, deterministic inbound admission control.
 *
 * No I/O and no host imports: the decision is a function of the plugin config
 * and the inbound message facts only. The tombstone check runs first so the
 * access decision always precedes any media download (DESIGN.md §0).
 *
 * Import note: sibling modules use explicit `.ts` specifiers because this
 * package runs via `node --test --experimental-strip-types` (no emitted .js).
 */

import type {
  AccountId,
  AdmissionDecision,
  AdmissionReason,
  ChatJid,
  SenderId,
} from "./contracts.ts";
import { stableHash32 } from "./contracts.ts";

export interface AdmissionConfig {
  readonly dmPolicy: 'disabled' | 'allowlist' | 'open';
  readonly groupPolicy: 'disabled' | 'allowlist';
  readonly dmAllowFrom: readonly string[];
  readonly groupAllowFrom: readonly string[];
  readonly groupSenderAllowFrom: readonly string[];
  readonly requireMentionInGroups: boolean;
}

export interface AdmissionInput {
  readonly accountId: AccountId;
  readonly chatJid: ChatJid;
  readonly senderId?: SenderId;
  readonly isGroup: boolean;
  readonly mentioned: boolean;
  readonly tombstone?: boolean;
}

/** Exact literal match, with the single special entry `'*'` matching anything. */
function matchesAllowlist(entries: readonly string[], value: string): boolean {
  for (const entry of entries) {
    if (entry === '*' || entry === value) return true;
  }
  return false;
}

/** Copy before sorting: config arrays are readonly and must never be mutated. */
function canonicalSorted(entries: readonly string[]): string[] {
  return [...entries].sort();
}

/** Hash a canonical projection of every config field; never expose raw values. */
function computePolicyFingerprint(config: AdmissionConfig): string {
  const canonical = {
    dmPolicy: config.dmPolicy,
    groupPolicy: config.groupPolicy,
    dmAllowFrom: canonicalSorted(config.dmAllowFrom),
    groupAllowFrom: canonicalSorted(config.groupAllowFrom),
    groupSenderAllowFrom: canonicalSorted(config.groupSenderAllowFrom),
    requireMentionInGroups: config.requireMentionInGroups,
  };
  return `policy-${stableHash32(JSON.stringify(canonical))}`;
}

function decision(
  policyFingerprint: string,
  admitted: boolean,
  reason: AdmissionReason,
): AdmissionDecision {
  return { admitted, reason, policyFingerprint };
}

export function admitInbound(
  config: AdmissionConfig,
  input: AdmissionInput,
): AdmissionDecision {
  const policyFingerprint = computePolicyFingerprint(config);

  // Security invariant: reject tombstones before any downstream media handling.
  if (input.tombstone === true) {
    return decision(policyFingerprint, false, 'tombstone');
  }

  if (!input.isGroup) {
    if (config.dmPolicy === 'disabled') {
      return decision(policyFingerprint, false, 'disabled');
    }
    if (config.dmPolicy === 'open') {
      return decision(policyFingerprint, true, 'admitted');
    }
    // dmPolicy === 'allowlist'
    if (
      input.senderId === undefined ||
      !matchesAllowlist(config.dmAllowFrom, input.senderId)
    ) {
      return decision(policyFingerprint, false, 'sender-not-allowed');
    }
    return decision(policyFingerprint, true, 'admitted');
  }

  // Group gates, ordered for deterministic reasons:
  // disabled -> group-not-allowed -> sender-not-allowed -> missing-mention.
  if (config.groupPolicy === 'disabled') {
    return decision(policyFingerprint, false, 'disabled');
  }

  // An empty groupAllowFrom is account-level gating only: it never rejects.
  if (
    config.groupAllowFrom.length > 0 &&
    !matchesAllowlist(config.groupAllowFrom, input.chatJid)
  ) {
    return decision(policyFingerprint, false, 'group-not-allowed');
  }

  if (config.groupSenderAllowFrom.length > 0) {
    if (
      input.senderId === undefined ||
      !matchesAllowlist(config.groupSenderAllowFrom, input.senderId)
    ) {
      return decision(policyFingerprint, false, 'sender-not-allowed');
    }
  }

  if (config.requireMentionInGroups && !input.mentioned) {
    return decision(policyFingerprint, false, 'missing-mention');
  }

  return decision(policyFingerprint, true, 'admitted');
}
