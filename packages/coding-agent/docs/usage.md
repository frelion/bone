# Using Bone

Bone is a local coding agent with built-in tools and locally stored resources.

## Interactive mode

The interface contains a startup header, conversation history, editor, and status footer. Type `/` in the editor for built-in commands, skills, and prompt templates.

| Command | Description |
|---|---|
| `/login`, `/logout` | Manage provider credentials |
| `/model` | Switch models |
| `/plan` | Enter or leave Plan Mode |
| `/settings` | Configure Bone |
| `/new`, `/tree`, `/fork`, `/clone` | Manage conversations |
| `/compact` | Compact context |
| `/reload` | Reload local skills, prompts, themes, keybindings, and context files |
| `/trust` | Save a project trust decision |
| `/quit` | Quit Bone |

Use `!command` to run a shell command and send its output to the model. Use `!!command` to run it without adding output to the conversation.

### Plan Mode

Use `/plan` when the goal and likely solution are clear but you want to approve the proposed changes before Bone implements them. While Plan Mode is active, Bone can inspect the workspace with `read`, `grep`, `find`, and `ls`, query development platforms with `forge_context`, `forge_query`, `forge_audit`, and `forge_watch`, and ask structured questions with `ask_user_question`; it cannot edit files, run shell commands, or perform Forge mutations.

Bone may investigate or ask clarifying questions before presenting a formal plan. A completed plan opens actions to execute it, request a revised full plan, or cancel. Executing a plan returns to Default mode, restores the tools that were active before planning, and immediately starts implementation. Ordinary chat replies such as `start` or `looks good` do not approve a plan.

`/plan` leaves Plan Mode while planning. If a proposal is awaiting approval, leaving Plan Mode cancels it. Mode changes and plan decisions are rejected while the agent is running; interrupt the current turn first.

### Structured questions

Bone can pause a Default or Plan mode turn to ask up to four structured questions. Each question offers concrete single- or multi-select choices plus an unlabeled custom-answer input. Custom text can stand alone or supplement selected options; supplemental text is returned in the answer's optional `notes` field. Options may include a Markdown preview; the interactive UI shows the focused option's description and preview in a fixed-height pane beside the choices on wide terminals and below them on narrow terminals. The questionnaire must be submitted as a complete set. Press Escape twice to cancel. After submission or cancellation, the result is recorded as the pending tool result and the same agent turn continues automatically.

Question requests and decisions are stored in the session tree. Reopening a session or navigating to a branch with an unanswered request restores the questionnaire. Print mode persists and emits the request, records a `no_ui` cancellation, and returns an explicit unavailable error so the model can ask in ordinary chat text instead.

## Sessions

Sessions are stored under `~/.bone/agent/sessions/` by default.

```bash
bone -c
bone --no-session
bone --name "my task"
bone --session <path|id>
bone --fork <path|id>
```

## Project trust

Bone asks before loading project-local settings and resources. Until a project is trusted, Bone does not load `.bone/settings.json`, `.bone/skills`, `.bone/prompts`, or `.bone/themes`.

For non-interactive modes, `defaultProjectTrust` controls the default. Use `--approve` or `--no-approve` for one invocation. `/trust` stores a decision in `~/.bone/agent/trust.json`.

## Context files

Bone loads `AGENTS.md` or `CLAUDE.md` from `~/.bone/agent/`, ancestor directories, and the current directory. Disable this with `--no-context-files` or `-nc`.

Use `.bone/SYSTEM.md` or `~/.bone/agent/SYSTEM.md` to replace the default system prompt. Use `APPEND_SYSTEM.md` in the same locations to append text instead.

## CLI reference

```bash
bone [options] [@files...] [messages...]
```

### Commands

```bash
bone update
bone setup
```

`bone update` updates Bone itself only. `bone install`, `bone remove`, `bone uninstall`, `bone list`, and `bone config` are unavailable. Package and extension update targets are rejected.

### Modes

| Flag | Description |
|---|---|
| `-p`, `--print` | Print a response and exit |
| `--mode json` | Output JSON lines |
| `--mode rpc` | Run RPC mode |
| `--export <in> [out]` | Export a session to HTML |

### Resources

| Flag | Description |
|---|---|
| `--skill <path>` | Load a local skill file or directory for this run |
| `--no-skills` | Disable skill discovery |
| `--prompt-template <path>` | Load a local prompt template file or directory |
| `--no-prompt-templates` | Disable prompt-template discovery |
| `--theme <path>` | Load a local theme file or directory |
| `--no-themes` | Disable theme discovery |
| `--no-context-files`, `-nc` | Disable context-file discovery |

There is no `--extension`, `-e`, or `--no-extensions` option. Bone never discovers extension files from paths, package manifests, npm/git packages, settings, or resource directories.

### Tools

| Flag | Description |
|---|---|
| `--tools <list>`, `-t <list>` | Allowlist built-in and Bone-owned custom tools |
| `--exclude-tools <list>`, `-xt <list>` | Disable listed tools |
| `--no-builtin-tools`, `-nbt` | Disable built-in tools |
| `--no-tools`, `-nt` | Disable all tools |

Built-in tools are `read`, `bash`, `edit`, `write`, `grep`, `find`, `ls`, `ask_user_question`, and the `forge_*` GitLab/GitHub tools. See [GitLab and GitHub integration](../../../docs/forge.md).

### Structured questions over RPC

`get_state` includes `questionState`. When an agent calls `ask_user_question`, RPC emits `question_asked` with the complete request and remains in `awaitingAnswer` until the client responds:

```json
{"id":"1","type":"answer_question","requestId":"...","answers":[{"questionIndex":0,"question":"Which mode?","kind":"option","answer":"Safe"}]}
{"id":"2","type":"cancel_question","requestId":"...","reason":"user"}
```

Successful responses emit `question_answered` or `question_cancelled`; invalid, stale, or duplicate request IDs return an RPC error. Clients must send the complete answer array and must not infer an answer when no interactive UI is available.

### Examples

```bash
bone "List all .ts files in src/"
bone -p "Summarize this codebase"
cat README.md | bone -p "Summarize this text"
bone --provider openai --model gpt-4o "Help me refactor"
bone --tools read,grep,find,ls -p "Review the code"
```

## Compatibility boundary

Pi extension packages, Pi manifests, Pi SDK aliases, npm/git resource packages, and old extension paths are unsupported. Existing `packages` and `extensions` settings fields and `.bone/npm` or `.bone/git` directories are deliberately preserved but never read, migrated, deleted, or reported as errors.

Use [Skills](skills.md), [Prompt Templates](prompt-templates.md), and [Themes](themes.md) for supported customization.
