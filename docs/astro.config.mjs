import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { remarkBasePath } from './remark-base-path.mjs';
import { remarkMermaid } from './remark-mermaid.mjs';
import { ogImage } from './og-image-integration.mjs';

const base = '/agent-wechat';
const site = 'https://thisnick.github.io';

// Pages from the previous layout, kept working for existing links.
const moved = {
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
  'reference/environment-vars': 'reference/configuration',
};

export default defineConfig({
  site,
  base,
  redirects: Object.fromEntries(
    Object.entries(moved).map(([from, to]) => [`/${from}`, `${base}/${to}${to ? '/' : ''}`]),
  ),
  markdown: {
    remarkPlugins: [remarkBasePath(base), remarkMermaid()],
  },
  integrations: [
    ogImage(),
    starlight({
      title: 'agent-wechat',
      description: 'Run WeChat on a server and connect it to AI agents.',
      customCss: ['./src/styles/custom.css'],
      components: {
        Head: './src/components/Head.astro',
      },
      social: {
        github: 'https://github.com/thisnick/agent-wechat',
      },
      sidebar: [
        { label: 'Set up', items: [
          { label: 'What you need', slug: 'setup/requirements' },
          { label: 'Install', slug: 'setup/install' },
          { label: 'Connect your account', slug: 'setup/connect' },
          { label: 'Keep it running', slug: 'setup/run' },
          { label: 'Migrate from the old container', slug: 'setup/migrate' },
        ] },
        { label: 'Use it', items: [
          { label: 'CLI recipes', slug: 'use/cli' },
          { label: 'Quoted replies', slug: 'use/quoted-replies' },
          { label: 'Manage groups', slug: 'use/groups' },
          { label: 'Send attachments', slug: 'use/media' },
          { label: 'Video quality', slug: 'use/video' },
          { label: 'OpenClaw', items: [
            { label: 'Quick setup', slug: 'openclaw/setup' },
            { label: 'Managed or external', slug: 'openclaw/modes' },
            { label: 'Who the bot answers', slug: 'openclaw/access' },
            { label: 'Agent tools', slug: 'openclaw/tools' },
            { label: 'Read receipts', slug: 'openclaw/read-receipts' },
          ] },
          { label: 'HTTP API', slug: 'use/api' },
        ] },
        { label: 'How it works', items: [
          { label: 'Architecture', slug: 'how-it-works/architecture' },
          { label: 'How wx talks to WeChat', slug: 'how-it-works/wechat' },
          { label: 'Sends and recovery', slug: 'how-it-works/sends' },
          { label: 'Message types', slug: 'how-it-works/message-types' },
          { label: 'Storage and backups', slug: 'how-it-works/storage' },
        ] },
        { label: 'Reference', items: [
          { label: 'CLI', slug: 'reference/cli' },
          { label: 'Configuration', slug: 'reference/configuration' },
          { label: 'API tokens', slug: 'reference/tokens' },
          { label: 'Troubleshooting', slug: 'reference/troubleshooting' },
          { label: 'Build from source', slug: 'reference/build' },
        ] },
      ],
    }),
  ],
});
