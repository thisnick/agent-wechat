import test from 'node:test';
import assert from 'node:assert/strict';
import { codeSkeleton, compareDocs, frontmatter, hash, headingIds, resolveLink, stamp } from './docs.mjs';

const en = `---
title: Install
---

import { Aside } from '@astrojs/starlight/components';
import shot from '../../assets/shot.svg';

Read [the API](/use/api/#routes) and [run it](../run/).

## Install it

\`\`\`bash
wx serve   # start the daemon
wx send --text "Hello"
\`\`\`

<Aside>Note</Aside>
`;
const api = '---\ntitle: API\n---\n\n## Routes\n';
const run = '---\ntitle: Run\n---\n\n<TabItem label="Docker" id="docker-compose">x</TabItem>\n';
const good = `---
title: 安装
sourceHash: ${hash(en)}
---

import { Aside } from '@astrojs/starlight/components';
import shot from '../../../assets/shot.svg';

阅读 [API](/use/api/#routes)，然后[运行](../run/#docker-compose)。

## 安装 \\{#install-it}

\`\`\`bash
wx serve   # 启动守护进程
wx send --text "你好"
\`\`\`

<Aside>注意</Aside>
`;
const pages = { 'setup/install.mdx': en, 'use/api.mdx': api, 'setup/run.mdx': run };

test('a faithful translation passes; comments, quoted text and import depth may differ', () => {
  const { status, problems } = compareDocs({ ...pages, 'zh-cn/setup/install.mdx': good }, ['zh-cn']);
  assert.deepEqual(problems, []);
  assert.deepEqual(status['zh-cn'].current, ['setup/install.mdx']);
  assert.deepEqual(status['zh-cn'].missing, ['use/api.mdx', 'setup/run.mdx']);
});

test('changed commands, components, links and anchors fail', () => {
  const bad = good
    .replace('wx serve', 'wx start')
    .replace('<Aside>注意</Aside>', '注意')
    .replace('/use/api/#routes', '/use/api/#路由')
    .replace('../run/#docker-compose', '/use/nothing/');
  const { problems } = compareDocs({ ...pages, 'zh-cn/setup/install.mdx': bad }, ['zh-cn']);
  assert.deepEqual(problems.map(problem => problem.message), [
    'code block 1 differs from English beyond comments and quoted text',
    'components differ from English (missing <Aside>)',
    'link /use/api/#路由: no heading with ID #路由',
    'link /use/nothing/ points to a page that does not exist',
  ]);
});

test('a stale translation is reported, not checked; an orphan page fails', () => {
  const stale = good.replace(`sourceHash: ${hash(en)}`, 'sourceHash: 000000000000').replace('wx serve', 'wx start');
  const { status, problems } = compareDocs({ ...pages, 'zh-cn/setup/install.mdx': stale, 'zh-cn/gone.mdx': '---\ntitle: x\n---\n' }, ['zh-cn']);
  assert.deepEqual(status['zh-cn'].stale, ['setup/install.mdx']);
  assert.deepEqual(status['zh-cn'].orphan, ['gone.mdx']);
  assert.deepEqual(problems.map(problem => problem.message), ['has no English page (remove it)']);
});

test('an unstamped translation is checked and reported as unrecorded', () => {
  const unstamped = good.replace(/sourceHash: .*\n/, '');
  const { status, problems } = compareDocs({ ...pages, 'zh-cn/setup/install.mdx': unstamped }, ['zh-cn']);
  assert.deepEqual(status['zh-cn'].unstamped, ['setup/install.mdx']);
  assert.deepEqual(problems, []);
});

test('helpers', () => {
  assert.deepEqual(frontmatter('---\ntitle: "A"\nsourceHash: abc\n---\nbody').fields, { title: 'A', sourceHash: 'abc' });
  assert.equal(frontmatter(stamp('---\ntitle: A\nsourceHash: old\n---\nbody', 'new')).fields.sourceHash, 'new');
  assert.deepEqual(headingIds('## Hello World\n## Hello World\n## 联系人 \\{#contacts}\n<a id="x"></a>'), ['x', 'hello-world', 'hello-world-1', 'contacts']);
  assert.equal(codeSkeleton('wx send --text "Hi" # note\n// comment\nrun(\'a\')'), 'wx send --text ""\n\nrun(\'\')');
  assert.deepEqual(resolveLink('../run/#tabs', 'setup/install.mdx'), { page: 'setup/run.mdx', anchor: 'tabs' });
  assert.deepEqual(resolveLink('/agent-wechat/', 'setup/install.mdx'), { page: 'index.mdx', anchor: '' });
  assert.equal(resolveLink('https://example.com', 'index.mdx'), null);
});
