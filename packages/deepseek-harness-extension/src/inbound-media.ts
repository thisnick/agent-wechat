/**
 * Inbound media pipeline (M3).
 *
 * Design boundary (DESIGN.md §3.1): media bytes are only touched AFTER
 * admission. The pipeline here runs strictly on admitted messages and never
 * downloads anything itself — it renders trusted metadata into the followup
 * text so the model knows media exists and can fetch bytes on demand with
 * `wechat_media` (which re-validates scope before touching bytes).
 *
 * Voice: without a configured SpeechProvider the message is annotated with the
 * explicit "transcription unavailable" note (DESIGN.md §2 mapping table), the
 * same fail-closed semantics as OpenClaw `native-history.ts:12-18`. No
 * fabricated transcript is ever produced.
 */

import type { InboundMedia, InboundMessage } from './contracts.ts';

/** Extra text appended to the followup for an inbound message with media. */
export function describeInboundMedia(media: readonly InboundMedia[]): string {
  const segments = media.map(describeMedia);
  return segments.length === 0 ? '' : segments.join('\n');
}

function describeMedia(item: InboundMedia): string {
  switch (item.kind) {
    case 'voice':
      return `[Voice message: ${item.mimeType} — transcription unavailable: no speech provider configured]`;
    case 'image':
      return `[Image attachment: ${item.mimeType}, asset ${item.assetId} — fetch with wechat_media]`;
    case 'file':
      return `[File attachment: ${item.mimeType}, asset ${item.assetId} — fetch with wechat_media]`;
    case 'sticker':
      return `[Sticker: asset ${item.assetId}]`;
  }
}

/**
 * Followup text for an inbound message: original caption/text plus media
 * annotations. Text-only messages pass through unchanged.
 */
export function inboundFollowupText(message: InboundMessage): string {
  const mediaNotes = describeInboundMedia(message.media);
  if (mediaNotes === '') return message.text;
  const base = message.text === '' ? '' : `${message.text}\n`;
  return `${base}${mediaNotes}`;
}
