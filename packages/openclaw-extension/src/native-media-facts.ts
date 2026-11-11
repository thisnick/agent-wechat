import type { NativeSuppliedAsset } from "./native-consumer-state.ts";

/** One attachment in OpenClaw's inbound media contract (`ctx.media`). */
export type NativeMediaFact = {
  path: string; url: string; contentType: string;
  kind: "image" | "audio" | "video" | "document";
  fileName?: string; messageId?: string;
  /** The voice note was already transcribed; OpenClaw does not transcribe it again. */
  transcribed?: true;
  /** Listed and staged, but not sent to the model as native image input. */
  hydrationSuppressed?: true;
};

/**
 * The media facts for a turn's supplied assets, in manifest order.
 *
 * OpenClaw lists every fact for the agent and copies it into a sandboxed agent's
 * workspace, so attachments reach agents that cannot read gateway paths. Only the
 * selected runtime image becomes native model input: other images are suppressed,
 * which keeps the single image slot of a catch-up turn. When the model image is a
 * prepared still (WXGF or a downsized preview), the original follows it as a
 * separate suppressed fact so the agent can still open or forward it. An image
 * sent as a file is a suppressed image too: as a document, OpenClaw's file
 * extraction would turn it into native image input.
 */
export function nativeMediaFacts(assets: readonly NativeSuppliedAsset[], transcribedMessageIds: ReadonlySet<string> = new Set()): NativeMediaFact[] {
  const facts: NativeMediaFact[] = [];
  for (const asset of assets) {
    const kind = asset.kind !== "file" ? asset.kind : asset.mime.startsWith("image/") ? "image" as const : "document" as const;
    const shared = { kind,
      ...(asset.fileName ? { fileName: asset.fileName } : {}), ...(asset.messageId ? { messageId: asset.messageId } : {}) };
    facts.push({ path: asset.path, url: asset.path, contentType: asset.mime, ...shared,
      ...(asset.kind === "audio" && asset.messageId && transcribedMessageIds.has(asset.messageId) ? { transcribed: true as const } : {}),
      ...(kind === "image" && !asset.runtimeMedia ? { hydrationSuppressed: true as const } : {}) });
    if (asset.role === "model_image" && asset.sourcePath && asset.sourcePath !== asset.path) {
      facts.push({ path: asset.sourcePath, url: asset.sourcePath, contentType: asset.sourceMime ?? asset.mime, ...shared, hydrationSuppressed: true });
    }
  }
  return facts;
}
