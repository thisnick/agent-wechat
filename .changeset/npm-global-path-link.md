---
"@agent-wechat/cli": minor
---

Put `wx` on `PATH` during global npm installs when npm's global bin folder is not on `PATH` by linking `~/.local/bin/wx` to npm's `wx` launcher (when `~/.local/bin` is on `PATH` and has no existing file or foreign symlink), warning when another `wx` with a different version is already on `PATH`, or printing the shell profile `PATH` line. Expose the same behavior as `wx-native-install --link` for pnpm and `--ignore-scripts` installs.
