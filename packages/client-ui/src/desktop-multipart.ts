/** WebKit cannot always expose a FormData Request.body containing Blob parts.
 * A multipart Blob gives the native HTTP bridge an ordinary readable stream.
 */
export function desktopMultipart(form: FormData): {
  body: Blob;
  contentType: string;
} {
  const boundary = `----agentwechat-${crypto.randomUUID()}`;
  const quote = (value: string) =>
    value.replace(/\r/g, "%0D").replace(/\n/g, "%0A").replace(/"/g, "%22");
  const parts: BlobPart[] = [];
  for (const [name, value] of form) {
    let header = `--${boundary}\r\nContent-Disposition: form-data; name="${quote(
      name
    )}"`;
    if (typeof value === "string")
      parts.push(
        `${header}\r\n\r\n`,
        value.replace(/\r\n|\r|\n/g, "\r\n"),
        "\r\n"
      );
    else {
      header += `; filename="${quote(value.name)}"\r\nContent-Type: ${
        value.type || "application/octet-stream"
      }`;
      parts.push(`${header}\r\n\r\n`, value, "\r\n");
    }
  }
  parts.push(`--${boundary}--\r\n`);
  const contentType = `multipart/form-data; boundary=${boundary}`;
  return { body: new Blob(parts, { type: contentType }), contentType };
}
