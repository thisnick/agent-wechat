/**
 * Registration seam between plugin-owned WeChat tools and the DSH tool
 * registry (`ctx.tools.register`). The tool layer stays decoupled from DSH
 * types; this module owns the mapping:
 *
 * - name/description/parameters pass through (the model-facing parameters
 *   schema is not schema-enforced by the registry);
 * - output schemas are already authored inside the enforced JsonSchemaNode
 *   subset (type/properties/required/items/additionalProperties/enum/const);
 * - text blocks pass through unchanged;
 * - image blocks resolve through the media sink's attachment table and
 *   degrade to an explicit text placeholder when the reference is unknown —
 *   a fabricated ImageBlock would lie about bytes that were never stored.
 */
import type { MediaSinkLike, ToolContentBlock, ToolRunContextLike, WechatToolDefinition } from './shared.ts';

/** Structural mirror of the subset of `dsh-tools` ToolDefinition we produce. */
export interface RegistrableToolDefinition {
  readonly name: string;
  readonly description: string;
  readonly parameters: Record<string, unknown>;
  readonly output: {
    readonly schema: Record<string, unknown>;
    readonly render: (args: unknown, value: unknown) => unknown[];
  };
  readonly execute: (args: unknown, exec: unknown) => Promise<unknown>;
}

/** Registers one tool definition and returns its exact unregister disposer. */
export type RegisterTool = (
  definition: RegistrableToolDefinition,
  options?: { readonly when: (context: unknown) => boolean | Promise<boolean> },
) => () => void;

/** Structural subset of the DSH step-time activation context. */
export interface ToolActivationContextLike {
  readonly agent?: { readonly id?: string } | undefined;
}

/** Image reference shape produced by the DSH attachment store. */
export interface RegisteredImageRef {
  readonly attachmentId: string;
  readonly mediaType: string;
  readonly bytes: number;
  readonly width: number;
  readonly height: number;
  readonly name?: string;
}

/** Structural subset of the DSH attachment store (`ctx.attachments`). */
export interface HostAttachmentsLike {
  saveImage(input: {
    readonly data: Uint8Array;
    readonly mediaType: string;
    readonly name?: string;
  }): Promise<unknown>;
  saveFile(input: { readonly data: Uint8Array; readonly name?: string }): Promise<unknown>;
}

/** Media types the attachment store accepts for images. */
const IMAGE_MEDIA_TYPES: ReadonlySet<string> = new Set([
  'image/png',
  'image/jpeg',
  'image/webp',
  'image/gif',
]);

/** Resolve the storage identifier out of an attachment reference. */
const attachmentIdOf = (ref: unknown): string | undefined => {
  if (ref === null || typeof ref !== 'object') return undefined;
  const id = (ref as { attachmentId?: unknown })['attachmentId'];
  return typeof id === 'string' ? id : undefined;
};

/**
 * Media sink backed by the real attachment store. It remembers every image
 * reference it published so the registration layer can turn a tool's
 * `{type:'image', attachment:{ref}}` block into a genuine ImageBlock.
 */
export function createHostMediaSink(
  attachments: HostAttachmentsLike,
): MediaSinkLike & { refOf(ref: string): RegisteredImageRef | undefined } {
  const images = new Map<string, RegisteredImageRef>();
  return {
    async saveImage({ data, mimeType, filename }) {
      if (!IMAGE_MEDIA_TYPES.has(mimeType)) {
        throw new Error(`unsupported image media type: ${mimeType}`);
      }
      const ref = (await attachments.saveImage({
        data,
        mediaType: mimeType,
        ...(filename === undefined ? {} : { name: filename }),
      })) as RegisteredImageRef;
      const id = attachmentIdOf(ref);
      if (id === undefined) {
        throw new Error('attachment store returned a reference without attachmentId');
      }
      images.set(id, ref);
      return { ref: id };
    },
    async saveFile({ data, filename }) {
      const ref = (await attachments.saveFile({ data, name: filename })) as {
        attachmentId?: unknown;
      };
      const id = attachmentIdOf(ref);
      if (id === undefined) {
        throw new Error('attachment store returned a file reference without attachmentId');
      }
      return { ref: id };
    },
    refOf(ref) {
      return images.get(ref);
    },
  };
}

/**
 * Project tool content blocks into registry content blocks. Image blocks
 * whose reference the sink never published degrade to an honest text note.
 */
export function projectBlocks(
  blocks: readonly ToolContentBlock[],
  resolveImageRef?: (ref: string) => unknown,
): unknown[] {
  return blocks.map((block) => {
    if (block.type !== 'image') return block;
    const attachment = resolveImageRef?.(block.attachment.ref);
    if (attachment === undefined) {
      return {
        type: 'text',
        text: `[WeChat image not available in this context: ${block.attachment.ref}]`,
      };
    }
    return { type: 'image', attachment };
  });
}

export interface RegisterWechatToolsOptions {
  readonly tools: readonly WechatToolDefinition[];
  readonly register: RegisterTool;
  /** Resolve an image reference published by the media sink; undefined degrades to text. */
  readonly resolveImageRef?: (ref: string) => unknown;
  /**
   * Per-step visibility. Returning false removes the tool from the model's
   * declarations AND from dispatch for that step (DESIGN.md M4: archive tools
   * stay model-invisible until explicitly granted). The checker must never
   * throw — a rejected condition fails the agent's step preparation.
   */
  readonly activation?: (
    toolName: string,
    context: ToolActivationContextLike,
  ) => boolean | Promise<boolean>;
}

/**
 * Register every WeChat tool with the DSH registry and return one combined
 * disposer that unregisters all of them (plugin unload / config reload).
 */
export function registerWechatTools(options: RegisterWechatToolsOptions): () => void {
  const disposers = options.tools.map((tool) =>
    options.register(
      {
        name: tool.name,
        description: tool.description,
        parameters: tool.parameters,
        output: {
          schema: tool.output.schema,
          render: (args: unknown, value: unknown) =>
            projectBlocks(tool.output.render(args, value), options.resolveImageRef),
        },
        // The registry hands a superset of ToolRunContextLike (agent + signal);
        // the tool layer only ever reads those two fields.
        execute: (args: unknown, exec: unknown) => tool.execute(args, exec as ToolRunContextLike),
      },
      options.activation === undefined
        ? undefined
        : {
            when: async (context: unknown) => {
              try {
                return await options.activation!(tool.name, context as ToolActivationContextLike);
              } catch {
                // Never fail step preparation over a visibility check.
                return false;
              }
            },
          },
    ),
  );
  return () => {
    for (const dispose of disposers) dispose();
  };
}
