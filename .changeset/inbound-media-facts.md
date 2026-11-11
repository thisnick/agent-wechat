---
"@agent-wechat/agent-wechat": patch
---

Pass every received attachment (images, video, voice notes and files) to OpenClaw as an attached file instead of writing its local path into the message text. OpenClaw now lists each attachment for the agent and copies it into a sandboxed agent's workspace, so sandboxed agents can open files people send in WeChat. A downsized or WXGF model image is followed by its original, and a transcribed voice note is marked so OpenClaw does not transcribe it again.
