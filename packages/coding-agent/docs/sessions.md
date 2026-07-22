# Conversations

Bone saves conversations as JSONL sessions so you can continue work, branch from earlier turns, and revisit previous paths.

## Storage

Conversations are stored under `~/.bone/agent/sessions/`, organized by working directory.

```bash
bone --continue                 # Continue the most recent conversation
bone --no-session              # Run without saving a conversation
bone --name "my task"          # Set the display name at startup
bone --session <path|id>       # Open a conversation by file or ID
bone --fork <path|id>          # Copy a stored conversation into this workspace
```

Use `/conversation` to show the current conversation ID, file, message count, token usage, and cost. For the JSONL schema and `SessionManager` API, see [Session Format](session-format.md).

## Commands

| Command | Description |
|---------|-------------|
| `/conversations` | Focus the conversation list in Side |
| `/new` | Start a new conversation |
| `/name <name>` | Set the current display name |
| `/conversation` | Show conversation information |
| `/tree` | Navigate the current conversation tree |
| `/fork` | Create a conversation from a previous user message |
| `/clone` | Duplicate the active branch |
| `/compact [prompt]` | Summarize older context; see [Compaction](compaction.md) |
| `/export [file]` | Export to HTML or JSONL |
| `/share` | Upload a private GitHub gist and show its share URL |

## Side

The conversation list is always available in the left pane. Use `Shift+Left` to focus it and `Shift+Right` to return to the transcript.

| Key | Action |
|-----|--------|
| `Up` / `Down` | Select a conversation |
| `Enter` | Open the selected conversation |
| `/` | Search loaded conversations and local memory |
| `d` | Delete the selected conversation, then `Enter` to confirm |
| `Escape` | Cancel search or delete confirmation |
| `PageUp` / `PageDown` | Scroll the transcript |

Deletion uses the platform trash mechanism when available. The matching local-memory record and last-active pointer are removed as part of the same workflow.

## Naming

Use `/name <name>` or the startup option:

```bash
bone --name "Refactor auth module"
```

Named conversations are easier to find from Side search.

## Branching

Conversation files store a tree. Every entry has an `id` and `parentId`, and the current position is the active leaf.

- `/tree` opens a selector for entries in the current conversation and moves the active leaf to the selected entry.
- `/fork` selects an earlier user message and creates a separate conversation from that point.
- `/clone` copies the complete active branch into a separate conversation.

The OpenTUI tree selector uses `Up`, `Down`, `Enter`, and `Escape`. When navigation returns editable text, Bone restores it to the composer only if the composer is empty.

## Format

Session files contain messages, model and thinking-level changes, labels, compactions, plan and question records, and branch summaries. See [Session Format](session-format.md) for the on-disk schema.
