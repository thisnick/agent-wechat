# Profile API naming

These instructions apply to this crate and its examples. Follow the repository's
root agent guide as well.

- Name new public operations that change an official profile `apply_*`, whether
  they update SQLite records, MMKV, media-cache files or several of these.
- Use `stage_*` for preparing a change without writing to the profile. Staging
  may read and validate existing files and return proposed bytes.
- Keep existing `publish_*` APIs and their current behavior for compatibility.
  Do not add new `publish_*` names or rename existing APIs merely to normalize
  this convention. Deliberate API migrations need a separate compatibility review.
- Do not infer transaction guarantees from a prefix. Document the files and
  records changed, partial-write behavior, replay behavior and caller obligations.
- These are this library's API names, not evidence of the official client's
  internal names or recovery implementation. Preserve the boundary between
  official storage-format work here and caller-owned coordination/policy.
