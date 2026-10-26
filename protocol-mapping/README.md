# Protocol mapping status

Track each feature from its official protocol entry point through saved profile
state and the daemon API. Keep a separate file per domain so a supported API
field is not mistaken for complete protocol support.

| Domain | Tracker | Current focus |
| --- | --- | --- |
| Contact updates | [Contact updates](contact-updates.md) | Incoming notification path, contact fields, profile merge and mute-state reads |
| Threads API | Separate tracker to be added | History, summaries, read state and events |
| Groups API | Separate tracker to be added | Group metadata, membership, changes and events |

Every tracker records the date, matching official build, implementation revision,
field-level status, evidence, remaining gaps and next validation. Distinguish
these independently:

- **Mapped:** matching-build evidence establishes a field or behavior.
- **Implemented:** repository code handles it; name the revision or branch.
- **Fixture tested:** synthetic tests exercise the named behavior.
- **Live verified:** a named official-client or standalone-daemon test observed it.
- **Partial / unknown:** record the boundary instead of assigning a meaning.

Keep credentials, account identifiers, private infrastructure, raw captures,
databases and screenshots outside this repository. This directory tracks public
implementation status and verification boundaries.
