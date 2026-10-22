# LAN login fixture

`lan-login.json` contains only synthetic bytes: a fixture device, private example
IP, login identifier, sequential 16-byte buffer and `synthetic-token`.
Its protobuf and framing bytes were encoded independently of the Rust codecs.
The existing BaseRequest profile includes a one-byte NUL session field.

The field mapping and default TCP framing were checked against the supplied
Linux 4.1.13.26 x86_64 artifact (SHA-256
`2749535f299ba67feb8ef80cab898bb0531fa8f376b16f8a72db706811c84132`):

- QR notification field17: SKBuffer containing risk-extension field1; value11
  starts the LAN challenge.
- CGI22312 request: BaseRequest1, LAN list2, current login identifier3, fresh
  16-byte SKBuffer4. LAN list field1 repeats entries with string address1 and
  varint port2. Response: BaseResponse1 and opaque token string2.
- Listener range: 8050 through 8100. Default packer: big-endian u32 equal to
  JSON length plus four, then JSON string fields `scene` and `token`.

This is static implementation evidence and a codec fixture, not a captured
successful login or a claim about phone/server activation and acceptance.
