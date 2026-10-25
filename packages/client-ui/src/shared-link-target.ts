export function sharedLinkTarget(value: string | null) {
  if (!value || !/^https?:\/\//i.test(value)) return null;
  try {
    const url = new URL(value);
    if (
      !["http:", "https:"].includes(url.protocol) ||
      url.username ||
      url.password
    )
      return null;
    return { href: url.href, host: url.host.replace(/^www\./i, "") };
  } catch {
    return null;
  }
}
