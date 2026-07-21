# Using Bone

Bone is a local coding agent with built-in tools and locally stored resources.

## Interactive mode

The interface contains a startup header, conversation history, editor, and status footer. Type `/` in the editor for built-in commands, skills, and prompt templates.

| Command | Description |
|---|---|
| `/login`, `/logout` | Manage provider credentials |
| `/model` | Switch models |
| `/settings` | Configure Bone |
| `/new`, `/tree`, `/fork`, `/clone` | Manage conversations |
| `/compact` | Compact context |
| `/reload` | Reload local skills, prompts, themes, keybindings, and context files |
| `/trust` | Save a project trust decision |
| `/quit` | Quit Bone |

Use `!command` to run a shell command and send its output to the model. Use `!!command` to run it without adding output to the conversation.

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

Built-in tools are `read`, `bash`, `edit`, `write`, `grep`, `find`, and `ls`.

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
