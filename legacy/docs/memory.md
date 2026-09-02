# Local memory and semantic search

**English** | [简体中文](zh-CN/memory.md)

Bone's memory is local and workspace-scoped. JSONL sessions are the source of
truth; LanceDB is a rebuildable derived store under:

```text
~/.bone/agent/memory/v1/<workspace-hash>/
```

Bone materializes conversation exchanges rather than every raw JSONL entry. An
exchange contains a user task and its final assistant response. Safe file paths,
component names, and commands are stored as separate references. System prompts,
tool raw output, terminal logs, patches, credentials, and secrets are excluded.

## Install semantic search explicitly

Keyword search works without an embedding model. To enable local semantic recall,
run:

```bash
bone setup
```

This downloads and verifies the fixed CPU GGUF model once. Normal `bone` startup
never downloads it. A same-process Bun Worker loads CrispEmbed/ggml through Bun
FFI and mmaps the model weights, keeping inference off the TUI thread and the
weights outside Bun's JavaScript heap.

## Indexing and status

New exchanges enter LanceDB synchronously after persistence. A local controller
polls only rows whose `embeddingState` is `pending`, embeds them in small batches,
and updates the same item to `ready`. It does not scan vectors in application
memory. LanceDB runs lexical and vector retrieval and combines candidates.

Run `/status` to inspect the current session, memory store state, pending/ready
embeddings, worker ownership, vector index mode, and semantic availability.
