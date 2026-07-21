# Settings

Bone reads JSON settings from these locations. Project settings override global settings after the project is trusted.

| Location | Scope |
|---|---|
| `~/.bone/agent/settings.json` | Global |
| `.bone/settings.json` | Project |

Use `/settings` for common options or edit the files directly.

## Project trust

Project settings and project-local resources are loaded only for trusted projects. Configure `defaultProjectTrust` as `"ask"`, `"always"`, or `"never"`, use `--approve` or `--no-approve` for one run, and use `/trust` to save a decision.

## Common settings

| Setting | Type | Description |
|---|---|---|
| `defaultProvider` | string | Default model provider |
| `defaultModel` | string | Default model ID |
| `defaultThinkingLevel` | string | Default thinking level |
| `theme` | string | Built-in or locally loaded theme name |
| `externalEditor` | string | Command used for the external editor |
| `defaultProjectTrust` | string | `ask`, `always`, or `never` |
| `enabledModels` | string[] | Model patterns for cycling |
| `sessionDir` | string | Session storage directory |
| `httpProxy` | string | Proxy URL for HTTP clients |
| `enableSkillCommands` | boolean | Register skills as `/skill:name` commands |

Nested settings such as `compaction`, `retry`, `terminal`, `images`, `markdown`, and `warnings` are also supported. The settings UI exposes their common values.

## Local resources

Use the following arrays for local files or directories:

| Setting | Type | Description |
|---|---|---|
| `skills` | string[] | Local skill files or directories |
| `prompts` | string[] | Local prompt-template files or directories |
| `themes` | string[] | Local theme files or directories |

Global paths resolve relative to `~/.bone/agent`; project paths resolve relative to `.bone`. Absolute paths and `~` are supported.

Bone also discovers resources in:

- `~/.bone/agent/skills/`, `~/.bone/agent/prompts/`, and `~/.bone/agent/themes/`
- `.bone/skills/`, `.bone/prompts/`, and `.bone/themes/` for trusted projects

Example:

```json
{
  "defaultProvider": "anthropic",
  "defaultModel": "claude-sonnet-4-20250514",
  "theme": "dark",
  "skills": ["~/workflows/skills"],
  "prompts": ["./prompts/review.md"],
  "themes": ["./themes"]
}
```

## Legacy fields

The old `packages` and `extensions` fields are retained only so existing settings files remain valid. Bone does not read, change, migrate, or report these fields. It also leaves `.bone/npm`, `.bone/git`, and prior installation directories untouched.

Bone does not support Pi extension packages, `package.json#pi` manifests, Pi SDK imports, extension aliases, or npm/git resource packages. Use local skills, prompts, and themes instead.
