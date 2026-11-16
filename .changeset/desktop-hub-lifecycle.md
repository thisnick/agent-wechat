---
"@agent-wechat/desktop": minor
"@agent-wechat/rust-crates": minor
---

The desktop app's local hub now runs only while the app needs it. It runs while **This device** is selected and stops when you switch to another hub, unless the new **Settings → Keep this device’s hub running** option is on; turn that on if the `wx` command or another app used this device's hub while the window showed a remote hub. Quitting with ⌘Q, from the Dock or by logging out now stops the hub too; before, only **Quit AgentWechat** in the tray or menu bar did, and the other ways left the hub running and holding its port. If an earlier session left its hub running, the app says so instead of failing to open the selected hub, and offers **Stop leftover hub**, which checks that the hub uses this app's data and key before stopping it.

`wx serve --exit-with-parent` stops the daemon gracefully once the process that started it exits. The desktop app starts its hub with it, so a crash or forced quit no longer leaves the hub behind. `wechat-platform` adds `ParentWatch`, which reports when a process's original parent has exited.
