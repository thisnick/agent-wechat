/**
 * Docs languages. `root` is English at the site root; each other key is a
 * translation whose pages live in `src/content/docs/<key>/`. Pages a translation
 * lacks fall back to English. Read by astro.config.mjs and `pnpm i18n`.
 */
export const locales = {
  root: { label: 'English', lang: 'en' },
  'zh-cn': { label: '简体中文', lang: 'zh-CN' },
};

/** Folder names of the translations. */
export const translatedLocales = Object.keys(locales).filter((key) => key !== 'root');
