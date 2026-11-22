import assert from 'node:assert/strict';
import { test } from 'node:test';
import { callTools } from './call.ts';
import { ToolAccessError } from '../tool-context.ts';
import type { ToolDeps } from './shared.ts';
import type { ToolPrincipal } from '../contracts.ts';

// ---------------------------------------------------------------------------
// Shared fakes
// ---------------------------------------------------------------------------

const ownerPrincipal: ToolPrincipal = {
  agentId: 'agent-1',
  routeKey: 'default:wxid_alice' as ToolPrincipal['routeKey'],
  accountId: 'default' as ToolPrincipal['accountId'],
  chatJid: 'wxid_alice' as ToolPrincipal['chatJid'],
  isOwner: true,
  grants: [
    { scope: 'current-chat', tools: [] },
    { scope: 'owner', tools: [] },
    { scope: 'archive', tools: [] },
    { scope: 'call', tools: ['wechat_call'] },
  ],
  generation: 1,
};

// Has the call grant (so context.require passes) but no owner authority —
// this is the combination that reaches the owner-caller check in the tool.
const memberPrincipal: ToolPrincipal = {
  ...ownerPrincipal,
  agentId: 'agent-2',
  isOwner: false,
};

const exec = (): { agent: { id: string }; signal: AbortSignal } => ({
  agent: { id: 'agent-1' },
  signal: new AbortController().signal,
});

type CallsState = {
  dials: string[];
  hangups: Array<string | undefined>;
  conferences: Array<{ groupId: string; invitees: string[] }>;
  invites: Array<{ callId: string; contactIds: string[] }>;
};

function makeDeps(principal: ToolPrincipal = ownerPrincipal, outbound: string[] = ['wxid_alice']): {
  deps: ToolDeps;
  state: CallsState;
} {
  const state: CallsState = { dials: [], hangups: [], conferences: [], invites: [] };
  const deps: ToolDeps = {
    context: {
      require: async (_agentId: string, toolName: string) => {
        // Mirror the real gate: wechat_call needs an explicit call grant.
        const granted = principal.grants.some(scope => scope.scope === 'call' && scope.tools.includes(toolName));
        if (!granted) throw new ToolAccessError(`${toolName}: not granted to this Agent`, toolName);
        return principal;
      },
      revalidate: async () => principal,
      allows: () => true,
      forAgent: async () => principal,
    } as unknown as ToolDeps['context'],
    client: {} as ToolDeps['client'],
    accountId: 'default' as ToolDeps['accountId'],
    log: () => {},
    now: () => 1_000,
    outboundToolAllowFrom: outbound,
    calls: {
      status: () => ({ state: 'idle', available: true, calls: [] }),
      dial: async contactId => {
        state.dials.push(contactId);
        return { status: 'accepted' };
      },
      conferenceStart: async (groupId, invitees) => {
        state.conferences.push({ groupId, invitees });
        return { status: 'accepted' };
      },
      invite: async (callId, contactIds) => {
        state.invites.push({ callId, contactIds });
        return { status: 'accepted' };
      },
      hangupCall: async callId => {
        state.hangups.push(callId);
        return callId !== undefined;
      },
    },
  };
  return { deps, state };
}

const tool = (deps: ToolDeps) => callTools(deps)[0]!;

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

test('status needs the call grant but not the owner caller', async () => {
  const granted = makeDeps(ownerPrincipal);
  assert.deepEqual(await tool(granted.deps).execute({ action: 'status' }, exec()), {
    state: 'idle',
    available: true,
    calls: [],
  });

  const ungranted = makeDeps({
    ...ownerPrincipal,
    grants: ownerPrincipal.grants.filter(scope => scope.scope !== 'call'),
  });
  await assert.rejects(
    () => tool(ungranted.deps).execute({ action: 'status' }, exec()),
    /wechat_call: not granted to this Agent/,
  );
});

test('dial requires an owner caller listed in outboundToolAllowFrom', async () => {
  const { deps, state } = makeDeps();
  assert.deepEqual(await tool(deps).execute({ action: 'dial', contactId: 'wxid_bob' }, exec()), { status: 'accepted' });
  assert.deepEqual(state.dials, ['wxid_bob']);

  const stranger = makeDeps({ ...ownerPrincipal, chatJid: 'wxid_stranger' as ToolPrincipal['chatJid'] });
  await assert.rejects(
    () => tool(stranger.deps).execute({ action: 'dial', contactId: 'wxid_bob' }, exec()),
    /calls from wxid_stranger are not listed in calls.voice.outboundToolAllowFrom/,
  );

  const member = makeDeps(memberPrincipal);
  await assert.rejects(
    () => tool(member.deps).execute({ action: 'dial', contactId: 'wxid_bob' }, exec()),
    (error: unknown) => error instanceof ToolAccessError && /starting calls requires the owner caller/.test(error.message),
  );
});

test('dial and invite validate their required arguments', async () => {
  const { deps } = makeDeps();
  await assert.rejects(() => tool(deps).execute({ action: 'dial' }, exec()), /dial requires contactId/);
  await assert.rejects(
    () => tool(deps).execute({ action: 'invite', callId: 'call-1' }, exec()),
    /invite requires at least one contactId/,
  );
  await assert.rejects(() => tool(deps).execute({ action: 'teleport' }, exec()), /unknown action: teleport/);
});

test('conference_start requires a chatroom group and invitees', async () => {
  const { deps, state } = makeDeps();
  await assert.rejects(
    () => tool(deps).execute({ action: 'conference_start', groupId: 'wxid_bob', contactIds: ['wxid_bob'] }, exec()),
    /requires a group chat id ending with @chatroom/,
  );
  await assert.rejects(
    () => tool(deps).execute({ action: 'conference_start', groupId: '123@chatroom' }, exec()),
    /requires at least one invitee/,
  );
  assert.deepEqual(await tool(deps).execute({ action: 'conference_start', groupId: '123@chatroom', contactIds: ['wxid_bob'] }, exec()), {
    status: 'accepted',
  });
  assert.deepEqual(state.conferences, [{ groupId: '123@chatroom', invitees: ['wxid_bob'] }]);
});

test('hangup without a live call reports no_call and never touches the daemon', async () => {
  const { deps, state } = makeDeps();
  const result = (await tool(deps).execute({ action: 'hangup' }, exec())) as { status: string; note: string };
  assert.equal(result.status, 'no_call');
  assert.match(result.note, /not a delivery receipt|daemon call state event/);
  assert.deepEqual(state.hangups, [undefined]);

  assert.deepEqual(await tool(deps).execute({ action: 'hangup', callId: 'call-9' }, exec()), {
    status: 'requested',
    note: result.note,
  });
  assert.deepEqual(state.hangups, [undefined, 'call-9']);
});

test('mutations revalidate the route before touching the coordinator', async () => {
  let revalidates = 0;
  const { deps } = makeDeps();
  const dashing = {
    ...deps,
    context: {
      ...(deps.context as unknown as {
        revalidate: () => Promise<ToolPrincipal>;
      }),
      revalidate: async () => {
        revalidates += 1;
        return ownerPrincipal;
      },
    } as unknown as ToolDeps['context'],
  } as ToolDeps;
  await tool(dashing).execute({ action: 'dial', contactId: 'wxid_bob' }, exec());
  assert.equal(revalidates, 1);
});
