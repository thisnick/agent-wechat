# Optional native codec notices

The `native-opus` feature builds libopus through opusic-c 1.6.1 and opusic-sys.
The workspace lockfile pins the exact transitive release. These are independently
maintained codec dependencies; no library from the official client is loaded.

- libopus and opusic-sys: retain [LICENSE-OPUS](LICENSE-OPUS), copied from the
  bundled libopus 1.6.1 COPYING file in opusic-sys 0.7.6. It includes the source
  and binary redistribution terms and patent-grant references.
- opusic-c: retain [LICENSE-OPUSIC](LICENSE-OPUSIC), its BSD 3-Clause license and
  Douman copyright notice.

Distributions containing the optional native codec must reproduce these notices
with the applicable binary artifacts. Distributions of the default portable
crate do not include the native codec. The repository's MIT license applies to
our media interfaces and adapters.
