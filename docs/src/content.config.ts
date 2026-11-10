import { defineCollection } from 'astro:content';
import { z } from 'astro/zod';
import { docsSchema } from '@astrojs/starlight/schema';

export const collections = {
  docs: defineCollection({
    schema: docsSchema({
      extend: z.object({
        /** On a translated page: which version of the English page it was translated from (see `pnpm i18n status`). */
        sourceHash: z.string().optional(),
      }),
    }),
  }),
};
