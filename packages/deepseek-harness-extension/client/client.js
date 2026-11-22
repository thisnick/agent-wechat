// Client half of @agent-wechat/deepseek-harness-extension: a visual settings
// page for the WeChat channel plugin, rendered in two host surfaces:
//   1. the Plugins page's `plugins.bundle.config` slot (keyed by package name),
//   2. the Settings panel's `settings.section` list slot (id we own).
// Configuration values are read from and written through the Host's
// `configForms` service (namespace = our Loader entry id, declared in
// cordis.patch.yml as `deepseek-harness-extension`), so every edit lands in the
// profile patch and is validated against the plugin's Config schema before it
// is applied. Values are never shown as secrets here: `tokenCredentialRef` and
// `apiKeyCredentialRef` name harness credentials; the secrets themselves live
// in the credential store.
//
// Module format: hand-written `window.__ModuleLoader__.load` module, exactly
// like dsh-connect-workbuddy / dsh-cost-meter — react and DSH client packages
// come from the browser module table via `require` and are never bundled.
window.__ModuleLoader__.load({
  id: '@agent-wechat/deepseek-harness-extension',
  factory(require) {
    var module = { exports: {} };
    var exports = module.exports;
    Object.defineProperty(exports, Symbol.toStringTag, { value: 'Module' });
    const react = require('react');
    const h = react.createElement;

    // ---------------------------------------------------------------------------
    // Localized copy (zh first: this deployment's primary UI language).
    // ---------------------------------------------------------------------------
    const zh = {
      navLabel: '微信通道',
      title: '微信通道配置',
      summaryPrefix: '微信通道',
      groupConn: '连接（WeChat daemon）',
      groupAdm: '消息准入',
      groupOwner: 'Owner 与工具授权',
      groupCalls: '语音通话（默认关闭）',
      mode: '运行模式',
      mode_external: 'external（外接 daemon，推荐）',
      mode_managed: 'managed（由插件托管 daemon）',
      endpoint: 'Hub 地址',
      endpointHint: 'WeChat daemon 的 HTTP 地址，例如 http://127.0.0.1:6790（需含 http:// 或 https://）',
      accountId: '账号 ID',
      accountIdHint: 'daemon 里的微信账号标识，默认 default',
      tokenRef: 'daemon 令牌凭据名',
      tokenRefHint: '保存 daemon 访问令牌的凭据引用（凭据库里的名字，不是令牌本身），默认 WECHAT_DAEMON_TOKEN',
      dmPolicy: '私聊准入',
      dm_disabled: 'closed（关闭）',
      dm_allowlist: 'allowlist（白名单）',
      dm_open: 'open（对所有私聊开放）',
      dmAllowFrom: '私聊白名单',
      groupPolicy: '群聊准入',
      grp_disabled: 'closed（关闭）',
      grp_allowlist: 'allowlist（白名单）',
      groupAllowFrom: '群聊白名单',
      groupSenderAllowFrom: '群成员白名单',
      mention: '群内需 @提及',
      ownerAllowFrom: 'Owner 账号',
      ownerAllowFromHint: '这些 wxid 的私聊视为账号主人，解锁 owner/voice 工具',
      archiveGrant: 'archive 工具授权',
      archiveGrantHint: '额外允许使用 wechat_archive_* 归档工具的 wxid（默认拒绝）',
      callGrant: '通话工具授权',
      callGrantHint: '额外允许使用 wechat_call 工具的 wxid（默认拒绝）',
      callsEnabled: '启用语音通话',
      callsAgent: '通话 Agent',
      callsAgentHint: '承载通话会话的 agent id（calls.voice.agentId）',
      callsInbound: '来电白名单',
      callsInboundHint: '允许自动接听的来电 wxid（calls.voice.inboundAllowFrom）',
      callsKeyRef: '语音服务 API Key 凭据名',
      callsKeyRefHint: '保存 OpenAI API Key 的凭据引用（calls.voice.realtime.apiKeyCredentialRef）',
      csvHint: '多个值用逗号分隔',
      csvPlaceholder: 'wxid_a, wxid_b',
      useDefault: '（默认）',
      save: '保存',
      discard: '放弃更改',
      saving: '保存中…',
      saveFailed: '保存失败：宿主拒绝了本次修改，请检查取值后重试。',
      invalidNote: '有字段未通过校验（红框标出），修正后再保存。',
      dirtyNote: '有未保存的修改。',
      loading: '正在读取配置…',
      unavailable: '配置服务不可用：插件未加载或宿主未提供设置表单。',
      readonly: '当前配置层不可写（被上层覆盖），只能查看。',
      reset: '重置',
      overriddenHint: '已被用户配置覆盖',
      summaryDm: '私聊',
      status_connected: '已连接',
      status_connecting: '连接中',
      status_parked: '未连接',
      status_stopped: '已停止',
      status_unknown: '状态未知',
      statusAttempt: '第 {0} 次重试',
      statusError: '错误',
      statusUnreachable: '无法读取插件状态（插件可能未加载）',
    };
    const en = {
      navLabel: 'WeChat channel',
      title: 'WeChat channel settings',
      summaryPrefix: 'WeChat channel',
      groupConn: 'Connection (WeChat daemon)',
      groupAdm: 'Message admission',
      groupOwner: 'Owner & tool grants',
      groupCalls: 'Voice calls (off by default)',
      mode: 'Run mode',
      mode_external: 'external (external daemon, recommended)',
      mode_managed: 'managed (plugin-hosted daemon)',
      endpoint: 'Hub address',
      endpointHint: 'HTTP address of the WeChat daemon, e.g. http://127.0.0.1:6790 (must include the scheme)',
      accountId: 'Account ID',
      accountIdHint: 'WeChat account id inside the daemon, default "default"',
      tokenRef: 'Daemon token credential',
      tokenRefHint: 'Credential reference that stores the daemon access token (a credential name, not the token), default WECHAT_DAEMON_TOKEN',
      dmPolicy: 'Direct messages',
      dm_disabled: 'closed',
      dm_allowlist: 'allowlist',
      dm_open: 'open to everyone',
      dmAllowFrom: 'DM allowlist',
      groupPolicy: 'Group chats',
      grp_disabled: 'closed',
      grp_allowlist: 'allowlist',
      groupAllowFrom: 'Group allowlist',
      groupSenderAllowFrom: 'Group sender allowlist',
      mention: 'Require @mention in groups',
      ownerAllowFrom: 'Owner accounts',
      ownerAllowFromHint: 'DMs from these wxids count as the account owner and unlock owner/voice tools',
      archiveGrant: 'Archive tool grant',
      archiveGrantHint: 'Extra wxids allowed to use the wechat_archive_* tools (denied by default)',
      callGrant: 'Call tool grant',
      callGrantHint: 'Extra wxids allowed to use the wechat_call tool (denied by default)',
      callsEnabled: 'Enable voice calls',
      callsAgent: 'Call agent',
      callsAgentHint: 'Agent id that hosts call sessions (calls.voice.agentId)',
      callsInbound: 'Inbound call allowlist',
      callsInboundHint: 'wxids whose calls may be answered automatically (calls.voice.inboundAllowFrom)',
      callsKeyRef: 'Speech API key credential',
      callsKeyRefHint: 'Credential reference holding the OpenAI API key (calls.voice.realtime.apiKeyCredentialRef)',
      csvHint: 'Separate multiple values with commas',
      csvPlaceholder: 'wxid_a, wxid_b',
      useDefault: '(default)',
      save: 'Save',
      discard: 'Discard changes',
      saving: 'Saving…',
      saveFailed: 'Saving failed: the host rejected this change. Check the values and try again.',
      invalidNote: 'Some fields failed validation (outlined in red). Fix them before saving.',
      dirtyNote: 'Unsaved changes.',
      loading: 'Loading configuration…',
      unavailable: 'Configuration service unavailable: the plugin is not loaded or the host serves no settings form.',
      readonly: 'This configuration layer is read-only (overridden by a higher layer).',
      reset: 'Reset',
      overriddenHint: 'Overridden by user configuration',
      summaryDm: 'DM',
      status_connected: 'Connected',
      status_connecting: 'Connecting',
      status_parked: 'Not connected',
      status_stopped: 'Stopped',
      status_unknown: 'Status unknown',
      statusAttempt: 'retry #{0}',
      statusError: 'error',
      statusUnreachable: 'Cannot read the plugin status (plugin may not be loaded)',
    };

    // ---------------------------------------------------------------------------
    // Field model. Every entry maps one visual control to one Config path.
    // ---------------------------------------------------------------------------
    const FIELDS = [
      { key: 'mode', path: ['native', 'mode'], kind: 'select', options: ['external', 'managed'], group: 'conn', optionPrefix: 'mode' },
      { key: 'endpoint', path: ['native', 'endpoint'], kind: 'endpoint', group: 'conn' },
      { key: 'accountId', path: ['native', 'accountId'], kind: 'text', group: 'conn' },
      { key: 'tokenRef', path: ['native', 'tokenCredentialRef'], kind: 'text', group: 'conn' },
      { key: 'dmPolicy', path: ['dmPolicy'], kind: 'select', options: ['disabled', 'allowlist', 'open'], group: 'adm', optionPrefix: 'dm' },
      { key: 'dmAllowFrom', path: ['dmAllowFrom'], kind: 'csv', group: 'adm' },
      { key: 'groupPolicy', path: ['groupPolicy'], kind: 'select', options: ['disabled', 'allowlist'], group: 'adm', optionPrefix: 'grp' },
      { key: 'groupAllowFrom', path: ['groupAllowFrom'], kind: 'csv', group: 'adm' },
      { key: 'groupSenderAllowFrom', path: ['groupSenderAllowFrom'], kind: 'csv', group: 'adm' },
      { key: 'mention', path: ['requireMentionInGroups'], kind: 'bool', group: 'adm' },
      { key: 'ownerAllowFrom', path: ['tools', 'ownerAllowFrom'], kind: 'csv', group: 'owner' },
      { key: 'archiveGrant', path: ['tools', 'grants', 'archive'], kind: 'csv', group: 'owner' },
      { key: 'callGrant', path: ['tools', 'grants', 'call'], kind: 'csv', group: 'owner' },
      { key: 'callsEnabled', path: ['calls', 'voice', 'enabled'], kind: 'bool', group: 'calls' },
      { key: 'callsAgent', path: ['calls', 'voice', 'agentId'], kind: 'text', group: 'calls' },
      { key: 'callsInbound', path: ['calls', 'voice', 'inboundAllowFrom'], kind: 'csv', group: 'calls' },
      { key: 'callsKeyRef', path: ['calls', 'voice', 'realtime', 'apiKeyCredentialRef'], kind: 'text', group: 'calls' },
    ];
    const GROUPS = [
      { key: 'groupConn', fields: FIELDS.filter((f) => f.group === 'conn') },
      { key: 'groupAdm', fields: FIELDS.filter((f) => f.group === 'adm') },
      { key: 'groupOwner', fields: FIELDS.filter((f) => f.group === 'owner') },
      { key: 'groupCalls', fields: FIELDS.filter((f) => f.group === 'calls') },
    ];

    function pathGet(obj, path) {
      let v = obj;
      for (const k of path) {
        if (v === null || typeof v !== 'object') return undefined;
        v = v[k];
      }
      return v;
    }

    function formatValue(kind, v) {
      if (v === undefined || v === null) return '';
      if (kind === 'csv') return Array.isArray(v) ? v.map(String).join(', ') : String(v);
      if (kind === 'bool') return v === true ? 'true' : v === false ? 'false' : '';
      return String(v);
    }

    // ---------------------------------------------------------------------------
    // Connection status (polled from the plugin's own status route).
    // ---------------------------------------------------------------------------
    function fmt(template, ...args) {
      return String(template).replace(/\{(\d+)\}/g, (m, i) => (args[Number(i)] === undefined ? m : String(args[Number(i)])));
    }

    function statusPhase(report) {
      if (report === undefined) return 'unknown';
      if (report.running === true) return report.serverId !== undefined ? 'connected' : 'connecting';
      if (typeof report.error === 'string' && report.error.length > 0) return 'parked';
      if ((report.attempts ?? 0) > 0) return 'connecting';
      return 'stopped';
    }

    const STATUS_COLORS = {
      connected: '#2aae67',
      connecting: '#d97706',
      parked: '#d54966',
      stopped: 'rgba(127,127,127,.55)',
      unknown: 'rgba(127,127,127,.35)',
    };

    function renderStatusStrip(t, controller) {
      const report = controller.statusReport();
      const phase = statusPhase(report);
      const details = [];
      if (report !== undefined) {
        if (phase === 'connected' && typeof report.endpoint === 'string' && report.endpoint.length > 0) details.push(report.endpoint);
        if (phase !== 'connected' && (report.attempts ?? 0) > 0) details.push(fmt(t('statusAttempt'), report.attempts));
        if (typeof report.error === 'string' && report.error.length > 0) {
          const err = report.error.length > 140 ? report.error.slice(0, 140) + '…' : report.error;
          details.push(t('statusError') + ': ' + err);
        }
      } else if (controller.isStatusUnreachable()) {
        details.push(t('statusUnreachable'));
      }
      return h('div', { className: 'wechatcfg-status', role: 'status' },
        h('span', { className: 'wechatcfg-dot', style: { background: STATUS_COLORS[phase] } }),
        h('span', null, t('status_' + phase)),
        details.length > 0 ? h('span', { className: 'wechatcfg-statusdetail' }, details.join(' · ')) : null);
    }

    /** Staged text → mutate op value. `clear` unsets (fall back to base/default). */
    function parseValue(field, text) {
      const s = String(text).trim();
      if (s === '') return { kind: 'clear' };
      if (field.kind === 'csv') {
        const items = [...new Set(s.split(/[,，]/).map((x) => x.trim()).filter((x) => x.length > 0))];
        return items.length > 0 ? { kind: 'set', value: items } : { kind: 'clear' };
      }
      if (field.kind === 'bool') {
        if (s === 'true') return { kind: 'set', value: true };
        if (s === 'false') return { kind: 'set', value: false };
        return { kind: 'invalid' };
      }
      if (field.kind === 'number') {
        const n = Number(s);
        return Number.isFinite(n) ? { kind: 'set', value: n } : { kind: 'invalid' };
      }
      if (field.kind === 'select') {
        return field.options.includes(s) ? { kind: 'set', value: s } : { kind: 'invalid' };
      }
      if (field.kind === 'endpoint') {
        try {
          const u = new URL(s);
          if (u.protocol !== 'http:' && u.protocol !== 'https:') return { kind: 'invalid' };
        } catch {
          return { kind: 'invalid' };
        }
        return { kind: 'set', value: s };
      }
      return { kind: 'set', value: s };
    }

    // ---------------------------------------------------------------------------
    // Controller: staged drafts over one Host ConfigFormController.
    // ---------------------------------------------------------------------------
    function createWechatConfigController() {
      let form;
      let formOff;
      let version = 0;
      let saving = false;
      let failed = false;
      let invalid = new Set();
      const draft = new Map(); // path key -> staged text ('' means unset)
      const listeners = new Set();
      // Connection status: polled from the read-only route the plugin registers
      // on its own fiber (`GET /plugins/wechat-channel/status`). Unknown until
      // the first successful poll; polling never breaks the settings page.
      const STATUS_URL = '/plugins/wechat-channel/status';
      const STATUS_POLL_MS = 5000;
      let status;
      let statusKey = '';
      let statusUnreachable = false;
      let statusTimer;
      const emit = () => {
        version += 1;
        for (const l of listeners) l();
      };
      const controller = {
        getVersion: () => version,
        subscribe(l) {
          listeners.add(l);
          return () => listeners.delete(l);
        },
        attach(next) {
          if (form === next) return;
          if (formOff) {
            try { formOff(); } catch {}
          }
          form = next;
          formOff = typeof next?.subscribe === 'function' ? next.subscribe(emit) : undefined;
          failed = false;
          emit();
        },
        detach() {
          if (formOff) {
            try { formOff(); } catch {}
          }
          formOff = undefined;
          form = undefined;
          emit();
        },
        snapshot() {
          return form ? form.getSnapshot() : { status: 'unavailable' };
        },
        originalText(field) {
          return formatValue(field.kind, pathGet(controller.snapshot().value, field.path));
        },
        stagedText(field) {
          const key = field.path.join('.');
          return draft.has(key) ? draft.get(key) : controller.originalText(field);
        },
        isDirty(field) {
          const key = field.path.join('.');
          return draft.has(key) && draft.get(key) !== controller.originalText(field);
        },
        isOverridden(field) {
          return pathGet(controller.snapshot().user, field.path) !== undefined;
        },
        isInvalid(field) {
          return invalid.has(field.path.join('.'));
        },
        edit(field, text) {
          draft.set(field.path.join('.'), text);
          invalid.delete(field.path.join('.'));
          failed = false;
          emit();
        },
        /** Stage a revert to the schema default (unset op on save). */
        reset(field) {
          draft.set(field.path.join('.'), '');
          emit();
        },
        discard() {
          draft.clear();
          invalid = new Set();
          failed = false;
          emit();
        },
        dirtyCount() {
          return FIELDS.filter((f) => controller.isDirty(f)).length;
        },
        invalidCount() {
          return invalid.size;
        },
        isSaving: () => saving,
        isFailed: () => failed,
        statusReport: () => status,
        isStatusUnreachable: () => statusUnreachable,
        async refreshStatus() {
          if (typeof fetch !== 'function') return;
          try {
            const res = await fetch(STATUS_URL, { cache: 'no-store' });
            if (!res.ok) throw new Error('HTTP ' + res.status);
            const next = await res.json();
            const key = JSON.stringify(next);
            statusUnreachable = false;
            if (key !== statusKey) {
              statusKey = key;
              status = next;
              emit();
            }
          } catch {
            statusKey = '';
            status = undefined;
            if (!statusUnreachable) {
              statusUnreachable = true;
              emit();
            }
          }
        },
        startStatus() {
          if (statusTimer !== undefined || typeof fetch !== 'function') return;
          void controller.refreshStatus();
          statusTimer = setInterval(() => void controller.refreshStatus(), STATUS_POLL_MS);
        },
        async save() {
          if (!form || saving) return;
          const ops = [];
          invalid = new Set();
          for (const f of FIELDS) {
            if (!controller.isDirty(f)) continue;
            const parsed = parseValue(f, controller.stagedText(f));
            if (parsed.kind === 'invalid') {
              invalid.add(f.path.join('.'));
              continue;
            }
            if (parsed.kind === 'clear') ops.push({ op: 'unset', path: f.path });
            else ops.push({ op: 'set', path: f.path, value: parsed.value });
          }
          if (invalid.size > 0) {
            failed = true;
            emit();
            return;
          }
          if (ops.length === 0) {
            draft.clear();
            emit();
            return;
          }
          saving = true;
          failed = false;
          emit();
          try {
            const result = await form.mutate(ops, form.getSnapshot().revision);
            if (result === false) failed = true;
            else {
              draft.clear();
              // A config change restarts the channel fiber; refresh soon so the
              // status strip reflects the new runtime within one poll.
              void controller.refreshStatus();
            }
          } catch {
            failed = true;
          }
          saving = false;
          emit();
        },
      };
      return controller;
    }

    // ---------------------------------------------------------------------------
    // Styles: small, theme-neutral, no external CSS assets.
    // ---------------------------------------------------------------------------
    const STYLE = `
.wechatcfg-root{display:flex;flex-direction:column;gap:18px;color:inherit;font-size:13px;line-height:1.55;max-width:760px}
.wechatcfg-h{margin:0;font-size:13px;font-weight:600;opacity:.85}
.wechatcfg-grid{display:grid;grid-template-columns:minmax(150px,210px) minmax(220px,1fr);gap:10px 18px;align-items:start}
.wechatcfg-label{padding-top:6px;font-weight:500;display:flex;align-items:baseline;gap:8px;flex-wrap:wrap}
.wechatcfg-reset{border:none;background:none;color:inherit;opacity:.55;cursor:pointer;text-decoration:underline;font-size:12px;padding:0}
.wechatcfg-reset:hover{opacity:.9}
.wechatcfg-input,.wechatcfg-select{width:100%;box-sizing:border-box;padding:5px 9px;border:1px solid rgba(127,127,127,.4);border-radius:6px;background:transparent;color:inherit;font:inherit}
.wechatcfg-input:focus-visible,.wechatcfg-select:focus-visible{outline:2px solid rgba(127,127,127,.5);outline-offset:-1px}
.wechatcfg-input:disabled,.wechatcfg-select:disabled{opacity:.55}
.wechatcfg-invalid{border-color:#d54966}
.wechatcfg-check{display:flex;align-items:center;gap:8px;padding-top:6px}
.wechatcfg-hint{opacity:.6;font-size:12px;margin-top:3px}
.wechatcfg-err{color:#d54966;font-size:12px;margin-top:3px}
.wechatcfg-actions{display:flex;align-items:center;gap:12px;flex-wrap:wrap;margin-top:2px}
.wechatcfg-btn{padding:5px 16px;border-radius:6px;border:1px solid rgba(127,127,127,.4);background:transparent;color:inherit;cursor:pointer;font:inherit}
.wechatcfg-btn:disabled{opacity:.45;cursor:default}
.wechatcfg-note{opacity:.65;font-size:12px}
.wechatcfg-warn{color:#d54966;font-size:12px}
.wechatcfg-summary{opacity:.8}
.wechatcfg-status{display:flex;align-items:center;gap:8px;padding:7px 10px;border:1px solid rgba(127,127,127,.25);border-radius:8px;background:rgba(127,127,127,.05);font-size:12px}
.wechatcfg-dot{width:8px;height:8px;border-radius:50%;flex:none}
.wechatcfg-statusdetail{opacity:.7;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
@media (max-width:640px){.wechatcfg-grid{grid-template-columns:1fr;gap:4px}}
`;

    // ---------------------------------------------------------------------------
    // Component.
    // ---------------------------------------------------------------------------
    const useSync = react.useSyncExternalStore;

    function WechatNavIcon(props) {
      const size = typeof props?.size === 'number' ? props.size : 16;
      return h('svg', {
        viewBox: '0 0 24 24', width: size, height: size, 'aria-hidden': true,
        style: { display: 'block', borderRadius: '4px', opacity: props?.active === false ? 0.75 : 1 },
      },
        h('path', { fill: '#2aae67', d: 'M9.5 3C5.36 3 2 5.86 2 9.38c0 1.97 1.03 3.73 2.64 4.9l-.66 2.1 2.36-1.19c.86.24 1.79.37 2.76.37h.32a6.2 6.2 0 0 1-.28-1.85c0-3.51 3.29-6.36 7.35-6.36.1 0 .2 0 .3.01C16.5 4.68 13.3 3 9.5 3Zm-2.6 3.1a.9.9 0 1 1 0 1.8.9.9 0 0 1 0-1.8Zm5.2 0a.9.9 0 1 1 0 1.8.9.9 0 0 1 0-1.8Z' }),
        h('path', { fill: '#7ec79f', d: 'M22 14.88c0-2.93-2.8-5.3-6.25-5.3s-6.25 2.37-6.25 5.3 2.8 5.3 6.25 5.3c.62 0 1.22-.08 1.78-.22l2.05 1.04-.57-1.82C20.94 18.15 22 16.63 22 14.88Zm-8.45-1.63a.78.78 0 1 1 0 1.56.78.78 0 0 1 0-1.56Zm4.4 0a.78.78 0 1 1 0 1.56.78.78 0 0 1 0-1.56Z' }));
    }

    function renderField(state, t, snap, field) {
      const staged = state.stagedText(field);
      const overridden = state.isOverridden(field);
      const invalid = state.isInvalid(field);
      const disabled = snap.writable === false || snap.status !== 'ready';
      let control;
      if (field.kind === 'bool') {
        control = h('label', { className: 'wechatcfg-check' },
          h('input', {
            type: 'checkbox', checked: staged === 'true', disabled: disabled,
            onChange: (e) => state.edit(field, e.target.checked ? 'true' : 'false'),
            'aria-label': t(field.key),
          }),
          h('span', null, t(field.key)));
      } else if (field.kind === 'select') {
        control = h('select', {
          className: 'wechatcfg-select' + (invalid ? ' wechatcfg-invalid' : ''),
          value: staged, disabled: disabled,
          onChange: (e) => state.edit(field, e.target.value),
          'aria-label': t(field.key),
        },
          h('option', { value: '' }, t('useDefault')),
          field.options.map((o) => h('option', { key: o, value: o }, t(field.optionPrefix + '_' + o))));
      } else {
        control = h('input', {
          type: 'text',
          className: 'wechatcfg-input' + (invalid ? ' wechatcfg-invalid' : ''),
          value: staged, disabled: disabled, placeholder: field.kind === 'csv' ? t('csvPlaceholder') : '',
          onChange: (e) => state.edit(field, e.target.value),
          'aria-label': t(field.key),
        });
      }
      const hintKey = field.key + 'Hint';
      const hint = t(hintKey);
      const cells = [
        h('div', { className: 'wechatcfg-label', key: 'label' },
          h('span', null, t(field.key)),
          overridden ? h('button', {
            type: 'button', className: 'wechatcfg-reset', title: t('overriddenHint'),
            onClick: () => state.reset(field), disabled: disabled,
          }, t('reset')) : null),
        h('div', { key: 'control' },
          control,
          invalid ? h('div', { className: 'wechatcfg-err' }, t('invalidNote')) : null,
          typeof hint === 'string' && hint !== hintKey ? h('div', { className: 'wechatcfg-hint' }, hint) : null),
      ];
      return h('div', { className: 'wechatcfg-field', key: field.path.join('.'), style: { display: 'contents' } }, cells);
    }

    function WechatConfigCard(props) {
      const state = props.state;
      const t = typeof props.t === 'function' ? props.t : (k) => (zh[k] ?? en[k] ?? k);
      if (typeof useSync === 'function') {
        useSync(state.subscribe, state.getVersion, state.getVersion);
      }
      const view = props.view;
      const snap = state.snapshot();
      const endpoint = state.originalText(FIELDS[1]) || '—';
      const dm = state.originalText(FIELDS[4]);
      if (view === 'summary') {
        const statusLabel = t('status_' + statusPhase(state.statusReport()));
        return h('span', { className: 'wechatcfg-summary' },
          t('summaryPrefix') + ' · ' + endpoint + (dm ? ' · ' + t('summaryDm') + ': ' + t('dm_' + dm) : '') + ' · ' + statusLabel);
      }
      const groups = GROUPS.map((g) => h(react.Fragment, { key: g.key },
        h('h3', { className: 'wechatcfg-h' }, t(g.key)),
        h('div', { className: 'wechatcfg-grid' }, g.fields.map((f) => renderField(state, t, snap, f)))));
      const dirty = state.dirtyCount();
      const savingNow = state.isSaving();
      const failed = state.isFailed();
      const invalidCount = state.invalidCount();
      const writable = snap.writable !== false && snap.status === 'ready';
      const notes = [];
      if (snap.status === 'loading') notes.push(h('span', { className: 'wechatcfg-note', key: 'loading' }, t('loading')));
      else if (snap.status === 'unavailable') notes.push(h('span', { className: 'wechatcfg-warn', key: 'unavail' }, t('unavailable')));
      else if (snap.writable === false) notes.push(h('span', { className: 'wechatcfg-note', key: 'ro' }, t('readonly')));
      if (failed) notes.push(h('span', { className: 'wechatcfg-warn', key: 'failed' }, t('saveFailed')));
      else if (invalidCount > 0) notes.push(h('span', { className: 'wechatcfg-warn', key: 'invalid' }, t('invalidNote')));
      else if (dirty > 0 && !savingNow) notes.push(h('span', { className: 'wechatcfg-note', key: 'dirty' }, t('dirtyNote')));
      return h('div', { className: 'wechatcfg-root' },
        h('style', null, STYLE),
        renderStatusStrip(t, state),
        groups,
        h('div', { className: 'wechatcfg-actions' },
          h('button', {
            type: 'button', className: 'wechatcfg-btn', disabled: !writable || dirty === 0 || savingNow || invalidCount > 0,
            onClick: () => { void state.save(); },
          }, t(savingNow ? 'saving' : 'save')),
          dirty > 0 && !savingNow ? h('button', { type: 'button', className: 'wechatcfg-btn', onClick: () => state.discard() }, t('discard')) : null,
          notes));
    }

    // ---------------------------------------------------------------------------
    // apply: locale + slot registrations, all defensive (a missing surface or a
    // missing configForms service degrades to a console error, never a crash).
    // ---------------------------------------------------------------------------
    const inject = ['slots', 'locale'];
    const PKG_NAME = '@agent-wechat/deepseek-harness-extension';
    const SECTION_ID = 'wechat-channel';
    const NS = 'wechat.channel.config';
    // describe() keys a form by the Loader entry id; cover the spellings a
    // profile patch could produce (workbuddy's lesson), then fall back to a
    // pattern match over whatever is actually served.
    const NS_CANDIDATES = ['deepseek-harness-extension', 'include:deepseek-harness-extension', PKG_NAME];

    function apply(ctx) {
      try {
        ctx.effect(() => ctx.locale.register(NS, { zh, en }), 'wechat-config: copy');
        const t = ctx.locale.bind(NS);
        const controller = createWechatConfigController();
        controller.startStatus();
        const slots = ctx.slots;
        let liveDisposers = [];
        const clearLive = () => {
          for (const d of liveDisposers) {
            try { d(); } catch {}
          }
          liveDisposers = [];
        };
        const injectProps = () => ({ state: controller });
        const registerAll = () => {
          clearLive();
          try {
            liveDisposers.push(slots.inject('plugins.bundle.config', () => slots.register(
              { name: 'plugins.bundle.config', key: PKG_NAME, locale: NS, inject: injectProps },
              WechatConfigCard)));
          } catch (error) {
            console.error('[wechat-config] plugins.bundle.config registration failed:', error);
          }
          try {
            liveDisposers.push(slots.inject('settings.section', () => {
              const inner = [];
              inner.push(slots.register(
                { name: 'settings.section', id: SECTION_ID, order: 35, label: () => t('navLabel'), locale: NS, inject: injectProps },
                WechatConfigCard));
              try {
                const iconInject = slots.inject('settings.section.icon', () => slots.register(
                  { name: 'settings.section.icon', id: SECTION_ID, locale: NS },
                  WechatNavIcon));
                if (typeof iconInject === 'function') inner.push(iconInject);
              } catch (error) {
                console.error('[wechat-config] settings nav icon failed (page unaffected):', error);
              }
              return () => {
                for (const d of inner) {
                  try { d(); } catch {}
                }
              };
            }));
          } catch (error) {
            console.error('[wechat-config] settings.section registration failed:', error);
          }
        };
        const forms = typeof ctx.get === 'function' ? ctx.get('configForms') : undefined;
        if (forms && typeof forms.whileServed === 'function') {
          ctx.effect(() => forms.whileServed(NS_CANDIDATES, (served) => {
            const ns = NS_CANDIDATES.find((candidate) => served.has(candidate))
              ?? [...served].find((name) => /wechat|deepseek-harness/i.test(name));
            controller.attach(ns === undefined ? undefined : forms.get(ns));
            // Re-register on the served edge: the renderer caches inject results
            // by identity, so late binding only works if old entries are disposed.
            registerAll();
            return () => {
              clearLive();
              controller.detach();
            };
          }), 'wechat-config: settings namespace binding');
        } else if (forms && typeof forms.get === 'function') {
          controller.attach(forms.get(NS_CANDIDATES[0]));
        }
        registerAll();
      } catch (error) {
        console.error('[wechat-config] client half failed to load (host unaffected):', error);
      }
    }

    exports.inject = inject;
    exports.apply = apply;
    return module.exports;
  },
});
