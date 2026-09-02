# Session Format

Bone stores conversations as JSONL files under `~/.bone/agent/sessions/` by default. A session begins with a session header followed by message, model, thinking-level, compaction, branch, and metadata entries.

The session format is an internal, versioned implementation format rather than a compatibility boundary. Bone accepts only the current session version and rejects older versions, malformed JSONL, and invalid Exchange metadata instead of migrating or partially loading them. Consumers may preserve unknown fields within the current version, but must not assume cross-version compatibility.

Current sessions use version 4. Assistant messages associated with an Exchange persist `exchangeId`, `modelTurnId`, and `responseDisposition`; `responseDisposition` is one of `continuation`, `final`, or `rejected`. User inputs associated with an Exchange persist their delivery as `prompt`, `steer`, or `follow_up`.

Use `/export` for a user-facing HTML or JSONL export, and use the public `SessionManager` APIs when embedding Bone.

Plan proposals and structured questions use append-only domain entries. Structured question requests are written as `question_asked`; the matching terminal entry is either `question_answered` or `question_cancelled`, keyed by request ID. Bone replays these entries along the selected branch to restore `questionState`, including across compaction and tree navigation. Consumers should not remove these entries merely because their surrounding messages were compacted.
