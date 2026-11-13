import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { remarkBasePath } from './remark-base-path.mjs';
import { remarkHeadingIds } from './remark-heading-ids.mjs';
import { locales, translatedLocales } from './locales.mjs';
import { remarkMermaid } from './remark-mermaid.mjs';
import { ogImage } from './og-image-integration.mjs';

const base = '/agent-wechat';
const site = 'https://thisnick.github.io';

// Pages from the previous layout, kept working for existing links.
const moved = {
  'use/mentions': 'use/groups#mention-group-members',
  'use/video': 'use/quality',
  'use/contacts': 'use/api#contacts',
  'getting-started/overview': '',
  'getting-started/quickstart': 'setup/install',
  'getting-started/native-client': 'setup/requirements',
  'getting-started/migration': 'setup/migrate',
  'getting-started/cli/installation': 'setup/install',
  'getting-started/cli/commands': 'reference/cli',
  'getting-started/building-from-source': 'reference/build',
  'hosting/requirements': 'setup/requirements',
  'hosting/self-hosting': 'setup/run',
  'hosting/docker-compose': 'setup/run',
  'hosting/distribution': 'setup/install',
  'integrations/openclaw/setup': 'openclaw/setup',
  'integrations/openclaw/configuration': 'openclaw/access',
  'operations/tokens': 'reference/tokens',
  'operations/data': 'how-it-works/storage',
  'operations/restarting': 'how-it-works/sends',
  'reference/api': 'use/api',
  'use/quoted-replies': 'use/cli',
  'use/downloads': 'use/settings',
  'reference/environment-vars': 'reference/configuration',
  'reference/rust-protocol': 'reference/rust-libraries#send-one-text-message',
  'reference/rust-profile': 'reference/rust-libraries#profile-writes',
  'reference/calls-architecture': 'how-it-works/rust-crates',
  'reference/conference-calls': 'reference/calls#conference-calls',
  'openclaw/call-conversation': 'openclaw/calling#models-and-behavior',
};

export default defineConfig({
  site,
  base,
  redirects: Object.fromEntries(
    Object.entries(moved).flatMap(([from, to]) => {
      const [path, fragment] = to.split('#');
      const target = (prefix) => `${base}${prefix}/${path}${path ? '/' : ''}${fragment ? `#${fragment}` : ''}`;
      return ['', ...translatedLocales.map((locale) => `/${locale}`)].map((prefix) => [`${prefix}/${from}`, target(prefix)]);
    }),
  ),
  markdown: {
    remarkPlugins: [remarkBasePath(base, translatedLocales), remarkHeadingIds(), remarkMermaid()],
  },
  integrations: [
    ogImage(),
    starlight({
      title: 'agent-wechat',
      description: 'Run WeChat on a server and connect it to AI agents.',
      defaultLocale: 'root',
      locales,
      customCss: ['./src/styles/custom.css'],
      components: {
        Head: './src/components/Head.astro',
        MarkdownContent: './src/components/MarkdownContent.astro',
      },
      social: {
        github: 'https://github.com/thisnick/agent-wechat',
      },
      sidebar: [
        { label: 'Set up', translations: { 'zh-CN': '准备与安装' }, items: [
          { label: 'What you need', translations: { 'zh-CN': '准备条件' }, slug: 'setup/requirements' },
          { label: 'Install', translations: { 'zh-CN': '安装' }, slug: 'setup/install' },
          { label: 'Connect your account', translations: { 'zh-CN': '连接账号' }, slug: 'setup/connect' },
          { label: 'Desktop app', translations: { 'zh-CN': '桌面应用' }, slug: 'setup/desktop' },
          { label: 'Keep it running', translations: { 'zh-CN': '后台持续运行' }, slug: 'setup/run' },
          { label: 'Migrate from the old container', translations: { 'zh-CN': '从旧容器迁移' }, slug: 'setup/migrate' },
        ] },
        { label: 'Use it', translations: { 'zh-CN': '使用指南' }, items: [
          { label: 'CLI recipes', translations: { 'zh-CN': 'CLI 常用命令' }, slug: 'use/cli' },
          { label: 'Manage groups', translations: { 'zh-CN': '管理群聊' }, slug: 'use/groups' },
          { label: 'Send attachments', translations: { 'zh-CN': '发送附件' }, slug: 'use/media' },
          { label: 'Use and manage stickers', slug: 'use/stickers' },
          { label: 'Settings', translations: { 'zh-CN': '设置' }, slug: 'use/settings' },
          { label: 'Video and image quality', translations: { 'zh-CN': '视频与图片画质' }, slug: 'use/quality' },
          { label: 'OpenClaw', translations: { 'zh-CN': 'OpenClaw' }, items: [
            { label: 'Quick setup', translations: { 'zh-CN': '快速接入' }, slug: 'openclaw/setup' },
            { label: 'Managed or external', translations: { 'zh-CN': '托管与外部模式' }, slug: 'openclaw/modes' },
            { label: 'Calling', translations: { 'zh-CN': '语音通话' }, slug: 'openclaw/calling' },
            { label: 'Remember earlier calls', translations: { 'zh-CN': '记忆过往通话' }, slug: 'openclaw/call-memory' },
            { label: 'Who the bot answers', translations: { 'zh-CN': '响应权限控制' }, slug: 'openclaw/access' },
            { label: 'Agent tools', translations: { 'zh-CN': '智能体工具' }, slug: 'openclaw/tools' },
            { label: 'Manage Saved stickers', slug: 'openclaw/stickers' },
            { label: 'Read receipts', translations: { 'zh-CN': '已读回执' }, slug: 'openclaw/read-receipts' },
          ] },
          { label: 'HTTP API', translations: { 'zh-CN': 'HTTP API' }, slug: 'use/api' },
        ] },
        { label: 'How it works', translations: { 'zh-CN': '工作原理' }, items: [
          { label: 'Architecture', translations: { 'zh-CN': '架构设计' }, slug: 'how-it-works/architecture' },
          { label: 'Rust crate architecture', translations: { 'zh-CN': 'Rust Crate 架构' }, slug: 'how-it-works/rust-crates' },
          { label: 'How wx talks to WeChat', translations: { 'zh-CN': 'wx 与微信通信原理' }, slug: 'how-it-works/wechat' },
          { label: 'Sends and recovery', translations: { 'zh-CN': '消息发送与恢复' }, slug: 'how-it-works/sends' },
          { label: 'Message types', translations: { 'zh-CN': '消息类型' }, slug: 'how-it-works/message-types' },
          { label: 'Storage and backups', translations: { 'zh-CN': '存储与备份' }, slug: 'how-it-works/storage' },
          { label: 'Translations', translations: { 'zh-CN': '翻译如何保持更新' }, slug: 'how-it-works/translations' },
        ] },
        { label: 'Reference', translations: { 'zh-CN': '参考文档' }, items: [
          { label: 'CLI', translations: { 'zh-CN': 'CLI 命令' }, slug: 'reference/cli' },
          { label: 'Configuration', translations: { 'zh-CN': '配置选项' }, slug: 'reference/configuration' },
          { label: 'API tokens', translations: { 'zh-CN': 'API 令牌' }, slug: 'reference/tokens' },
          { label: 'Calling', translations: { 'zh-CN': '实时通话' }, slug: 'reference/calls' },
          { label: 'Calls API', translations: { 'zh-CN': '通话控制 API' }, slug: 'reference/calls-api' },
          { label: 'Call media API', translations: { 'zh-CN': '通话媒体 API' }, slug: 'reference/calls-media' },
          { label: 'Rust libraries', translations: { 'zh-CN': 'Rust 开发库' }, slug: 'reference/rust-libraries' },
          { label: 'Sticker API', slug: 'reference/stickers' },
          { label: 'Troubleshooting', translations: { 'zh-CN': '故障排查' }, slug: 'reference/troubleshooting' },
          { label: 'Build from source', translations: { 'zh-CN': '源码构建' }, slug: 'reference/build' },
          { label: 'Translate', translations: { 'zh-CN': '参与翻译' }, slug: 'reference/translate' },
        ] },
      ],
    }),
  ],
});
