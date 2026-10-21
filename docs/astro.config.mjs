import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { remarkBasePath } from './remark-base-path.mjs';
import { ogImage } from './og-image-integration.mjs';

const base = '/agent-wechat';
const site = 'https://thisnick.github.io';

export default defineConfig({
  site,
  base,
  markdown: {
    remarkPlugins: [remarkBasePath(base)],
  },
  integrations: [
    ogImage(),
    starlight({
      title: 'agent-wechat',
      description: 'A programmable WeChat interface for AI agents and automation.',
      customCss: ['./src/styles/custom.css'],
      components: {
        Head: './src/components/Head.astro',
      },
      social: {
        github: 'https://github.com/thisnick/agent-wechat',
      },
      sidebar: [
        { label: 'Getting Started', items: [
          { label: 'Overview', slug: 'getting-started/overview' },
          { label: 'Quick Start', slug: 'getting-started/quickstart' },
          { label: 'Supported Scope', slug: 'getting-started/native-client' },
          { label: 'Migration', slug: 'getting-started/migration' },
        ] },
        { label: 'CLI', items: [
          { label: 'Installation', slug: 'getting-started/cli/installation' },
          { label: 'Commands', slug: 'getting-started/cli/commands' },
          { label: 'Building from Source', slug: 'getting-started/building-from-source' },
        ] },
        { label: 'How It Works', items: [
          { label: 'Architecture', slug: 'how-it-works/architecture' },
          { label: 'Distribution Matrix', slug: 'hosting/distribution' },
        ] },
        { label: 'OpenClaw', items: [
          { label: 'Setup', slug: 'integrations/openclaw/setup' },
          { label: 'Configuration', slug: 'integrations/openclaw/configuration' },
        ] },
        { label: 'Hosting and Operations', items: [
          { label: 'Requirements', slug: 'hosting/requirements' },
          { label: 'Self-Hosting', slug: 'hosting/self-hosting' },
          { label: 'Docker Compose', slug: 'hosting/docker-compose' },
          { label: 'Tokens and Authentication', slug: 'operations/tokens' },
          { label: 'Data and Storage', slug: 'operations/data' },
          { label: 'Restarting and Recovery', slug: 'operations/restarting' },
        ] },
        { label: 'Reference', items: [
          { label: 'HTTP API', slug: 'reference/api' },
          { label: 'Environment Variables', slug: 'reference/environment-vars' },
          { label: 'Troubleshooting', slug: 'reference/troubleshooting' },
        ] },
      ],
    }),
  ],
});
