# Component compatibility fixtures

These unmodified upstream JavaScript files are used only in synthetic Mini Program
packages for browser acceptance tests. The tests exercise real computed properties
and MobX bindings through the runner, including state changes caused by a tap.
They do not add global libraries to user applications.

`packages.json` records each npm package's exact version, archive integrity and
source entry. Adjacent license files preserve the upstream notices.

| File | Upstream |
| --- | --- |
| `computed.js` | [miniprogram-computed](https://github.com/wechat-miniprogram/computed) 9.0.0 |
| `mobx.js` | [mobx-miniprogram](https://github.com/wechat-miniprogram/mobx) 6.12.3 |
| `bindings.js` | [mobx-miniprogram-bindings](https://github.com/wechat-miniprogram/mobx-miniprogram-bindings) 7.0.1 |
| `fast-deep-equal.js` | [fast-deep-equal](https://github.com/epoberezkin/fast-deep-equal) 3.1.3 |
| `rfdc.js` | [rfdc](https://github.com/davidmarkclements/rfdc) 1.4.1 |

The last two files are dependencies of the computed library. Refresh these fixtures
from the exact npm archives, verify their recorded integrity, and copy only the
listed entry files and licenses. Do not run package lifecycle scripts.

The optional `scripts/native-mini-component-reference.mjs` comparison runs the
same state and tap cases through upstream `j-component` 1.4.10 and its published
`miniprogram-exparser` 2.29.1 dependency. Set `MINI_COMPONENT_DEPS` to an isolated
`node_modules` containing those packages, the three Mini Program libraries above,
and esbuild. The browser is selected with the same Playwright environment
variables as `scripts/native-miniprogram-smoke.mjs`. This comparison uses simple
view/button adapters and establishes state/event behavior, not complete native
component compatibility or official-client rendering.
