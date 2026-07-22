# Session Format

Bone stores conversations as JSONL files under `~/.bone/agent/sessions/` by default. A session begins with a session header followed by message, model, thinking-level, compaction, branch, and metadata entries.

The session format is an implementation format for Bone conversations. Consumers should preserve unknown entry fields when reading or transforming session files so future Bone versions remain compatible.

Use `/export` for a user-facing HTML or JSONL export, and use the public `SessionManager` APIs when embedding Bone.

Plan proposals and structured questions use append-only domain entries. Structured question requests are written as `question_asked`; the matching terminal entry is either `question_answered` or `question_cancelled`, keyed by request ID. Bone replays these entries along the selected branch to restore `questionState`, including across compaction and tree navigation. Consumers should not remove these entries merely because their surrounding messages were compacted.
