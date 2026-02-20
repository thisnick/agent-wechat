# Changesets

This repo uses Changesets for npm package versioning.

Typical flow:

1. `pnpm changeset`
2. Commit the generated file in `.changeset/`
3. `pnpm changeset:version`
4. Commit version/changelog updates
5. Tag and push `vX.Y.Z` to trigger release workflows
