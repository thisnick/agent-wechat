# Glass-easel runner dependencies

The two JavaScript modules are the upstream ES distributions from the MIT-licensed
`glass-easel` and `glass-easel-miniprogram-adapter` npm packages at version 1.2.1.
See the [upstream source](https://github.com/wechat-miniprogram/glass-easel).

The adapter imports the adjacent local core module. Source map comments are
omitted because the CLI embeds only the browser runtime.

Original distribution hashes

- `glass_easel.all.es.js` SHA-256 `102510ed4edaa6f0de90b7052fce67812d8f07d4c6a3fdafaee5db777da8ef94`
- `glass_easel_miniprogram_adapter.es.js` SHA-256 `be38d267bd39b7dc37f61d66f34785198703d0059b7743f66ecf91c090f61edc`

## CSS syntax

`css-tree.js` is the unmodified ES distribution of CSS Tree 3.2.1, licensed
under MIT. The renderer uses its parser to adapt compiled Mini Program type
selectors to browser elements. See `CSS-TREE-LICENSE` and the
[upstream source](https://github.com/csstree/csstree).

- `csstree.esm.js` SHA-256 `a34589878a06f95e4e144c27748db5e6d81ba19619d3f36036027098e16c3522`
