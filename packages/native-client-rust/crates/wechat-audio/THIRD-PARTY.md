# Native audio adapter provenance

This internal crate adapts the MIT-licensed
[sys-voice 0.2.0](https://github.com/tigy32/sys-voice/tree/74ccfcfdf424bd8bef66ece8ac01f6d6cc093392)
and the [acpfx fork](https://github.com/thisnick/acpfx/tree/8ff29ec5fb6b1fa07c868f9760e227445ee62e32/packages/sys-voice).
The upstream copyright and license are retained in `LICENSE.sys-voice`.

Local changes:

- A single owned device worker disconnects and releases capture/playback resources
  on `stop()` or drop. Device opening is behind the `native-audio` feature.
- Playback has a sample-count limit and generation tokens. Clearing rejects old
  producers and discards old resampler history. New chunks reuse resampler state.
- Linux uses polled libpulse streams instead of blocking PulseAudio Simple reads,
  allowing worker shutdown even without new capture data. Playback fills the
  complete server write request, avoiding cumulative per-request polling delay. Explicit configured
  source/sink names are supported; global defaults and modules are never changed.
- macOS uses VoiceProcessingIO with paired render/capture callbacks. Custom device
  selection is rejected until implemented instead of silently using a default.
- Windows configures shared f32 mono streams and reports whether AEC was requested,
  unavailable, or unknown. Setup failure is not presented as successful AEC.
- Mobile backends and push-to-talk helpers are outside this fork's current scope.

No platform device or acoustic cancellation quality is established by the offline
unit tests. Native backend compilation and physical-device acceptance are separate
checks. PulseAudio AEC depends on the selected source/sink's server configuration;
VoiceProcessingIO/WASAPI setup does not prove measured echo suppression.
