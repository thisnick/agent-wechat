---
"@agent-wechat/cli": minor
"@agent-wechat/rust-crates": minor
"@agent-wechat/desktop": minor
"@agent-wechat/client-ui": minor
---

Open Mini Programs in a browser with `wx miniprogram serve APP_ID`.
Use `--message MESSAGE_ID` to open the page from a saved chat card.
In AgentWechat, open a card or pick an app from the Mini Programs list to
launch it in a separate window. CLI help and the desktop app mark Mini
Programs as experimental. The `read` token scope lists programs and shows cards.
Use `mini-programs` to launch cached programs, download their cached archives, and
call IPC methods such as `wx.login`. Launching from a saved card needs both scopes.
The hub verifies staged archives and can issue login codes when the
account, package and access key allow it. The runner supports local network
status, app visibility events, window-scoped storage, feedback UI, and
supported compiled Exparser components across template chunks in the same
staged archive, compiled stylesheet imports and responsive units, and isolated
page/component styles. Apply compiled app/page styles to Glass-easel pages without styling the host
toolbar. Support local navigator links and page titles, retain one Glass-easel
event backend across navigation, and expose current component IDs and typed
datasets for component lookup and click handlers. Update legacy DOM elements in
place so background data refreshes do not detach controls during a click.
Load full view templates separately from service logic and
report element visibility using the browser’s geometry. Render embedded images
and convert responsive units in inline styles as well as stylesheets. Load public
HTTPS display images and CSS backgrounds through a separate image-only bridge,
sharing a window-scoped cache without sending app credentials. Retrieve each app’s
request-domain policy through the authenticated hub and enforce it in the local
runner. Endpoint restrictions remain available for diagnostics.

Use `wx miniprogram fetch APP_ID` with a hub admin key to download and verify
a missing main release package through the selected WeChat account. Open
compiled configurations containing JavaScript string escapes, and support more
apps' module resolution and startup APIs.

Load JavaScript dependencies from verified, same-version staged subpackages
on demand and instantiate packaged WebAssembly. Apps with a Cloud Gateway
failure handler can fall back to the existing policy-checked request bridge.

Read legacy component declarations, initialize components before their first
property observers, and keep application error handlers from recursively
reporting their own failures.
Keep browser printing globals out of service modules so bundled libraries do
not mistake them for JavaScript-shell logging.

Retrieve list icons from WeChat app metadata when no saved card artwork is available.
Render Glass-easel image components using the same package and credential-free image
loader as Exparser, with image dimensions, aspect modes, and stale-load protection.
