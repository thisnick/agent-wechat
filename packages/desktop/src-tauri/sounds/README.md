# Desktop notification and ringtone sounds

## Soft pair (`AgentWechatSoftPair.wav`)

`AgentWechatSoftPair.wav` is an original synthesized notification chime, selected
for AgentWechat. It contains no recorded or third-party audio samples. The
360 ms WAV uses mono 48 kHz, 16-bit PCM with a peak level of about -13 dBFS.

## Soft ring (`AgentWechatSoftRing.wav`)

`AgentWechatSoftRing.wav` is an original synthesized incoming-call ringtone
(a double marimba motif), selected for AgentWechat. It contains no recorded or
third-party audio samples. The 3.0 s WAV uses mono 48 kHz, 16-bit PCM with a
peak level of about -12.5 dBFS.

macOS includes both WAVs at the app bundle's resource root. Linux sends their
installed paths to the notification service. Windows uses its native default
notification sound. When the main window is focused, OS incoming-call alerts are
suppressed so the app does not play a second audio stream over the native
notification.
