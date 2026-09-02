# Sessions and the Side panel

**English** | [简体中文](zh-CN/sessions.md)

Bone treats conversations as workspace-local working context. The Side panel is
the user-facing way to navigate them; JSONL session files remain implementation
details.

## Focus and navigation

- `Shift+Left` / `Shift+Right`: move focus between the conversation and Side.
- In Side focus: `↑` / `↓` selects a conversation; `Enter` opens it.
- `Ctrl+C` and `Ctrl+D` keep their normal clear/exit behavior even when Side has
  focus.

Opening another conversation does not interrupt a running one. A live session
continues in the background and its renderer/runtime is released after the run
settles, so inactive conversations do not accumulate active runtimes.

## Search and deletion

Use the Side search interaction to find workspace conversations. Keyword results
are available immediately; local semantic results appear when the optional
embedding model is installed.

In Side focus, press `d` to request deletion. `Enter` confirms and `Esc`
cancels. Deletion is soft: Bone first uses the system trash and otherwise moves
the JSONL file into its private session trash. A foreground conversation switches
to a neighbor, or a new empty conversation is created, before the old one moves.

## Conversation names

`/name <text>` sets a manual name. `/name` asks the configured title-generation
task model for a concise title without adding a chat message. `/model` controls
the conversation model and the title-generation model separately.
