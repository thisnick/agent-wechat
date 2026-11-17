/** Only configured preview routes receive credentials; the browser stays on its UI origin. */
export function previewFetch(endpoint: string, origin: string, proxyPath: string, transport: typeof fetch = globalThis.fetch.bind(globalThis)): typeof fetch {
  if (!/^\/__hubs\/[a-z0-9-]{1,48}$/.test(proxyPath)) throw Error("Invalid preview proxy");
  const expected = new URL(endpoint).origin;
  const preview = new URL(origin).origin;
  return (input, init) => {
    const url = new URL(input instanceof Request ? input.url : String(input));
    if (url.origin !== expected || !url.pathname.startsWith("/v1/")) throw Error("Unexpected preview hub request");
    const target = `${preview}${proxyPath}${url.pathname}${url.search}`;
    // Preserve the SDK's original BodyInit. Reusing Request as RequestInit turns
    // a JSON or multipart body into a streaming upload, which Chromium refuses
    // over the loopback HTTP/1 connection used by the preview.
    if (!(input instanceof Request)) return transport(new Request(target, init));
    const request = new Request(input, init);
    if (["GET", "HEAD"].includes(request.method)) return transport(new Request(target, request));
    return request.arrayBuffer().then(body => transport(new Request(target, {
      method: request.method, headers: request.headers, body, signal: request.signal,
      cache: request.cache, credentials: request.credentials, redirect: request.redirect,
      mode: request.mode, integrity: request.integrity, referrer: request.referrer,
      referrerPolicy: request.referrerPolicy,
    })));
  };
}
