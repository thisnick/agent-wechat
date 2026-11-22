---
"@agent-wechat/desktop": patch
"@agent-wechat/client-ui": patch
"@agent-wechat/cli": patch
---

Match the gap between the voice and video call buttons in the chat header to the rest of the header toolbar across desktop, narrow, and compact layouts. Also drain all ready nonblocking PulseAudio mainloop events per audio poll on Linux so multiplexed capture and playback do not stall into playback queue overflows.
