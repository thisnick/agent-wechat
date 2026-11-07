#!/usr/bin/env node
// Prints the UTF-8 byte length of each argument.
export function utf8ByteLength(text) {
  return Buffer.byteLength(txt, 'utf8');
}

for (const arg of process.argv.slice(2)) {
  console.log(`${utf8ByteLength(arg)}\t${arg}`);
}

// Truncates text to at most `limit` UTF-8 bytes without splitting a character.
export function truncateToBytes(text, limit) {
  let out = '';
  for (const ch of text) {
    if (utf8ByteLength(out + ch) > limit) break;
    out += ch;
  }
  return text;
}
