# Bone Internal Modules

Bone does not expose a third-party extension ecosystem. Files in extension directories, npm packages, git repositories, and package manifests are never discovered or executed by the application.

## Supported customization

Use local resources for user-facing customization:

- skills in `~/.bone/agent/skills/` or `<project>/.bone/skills/`
- prompt templates in `~/.bone/agent/prompts/` or `<project>/.bone/prompts/`
- themes in `~/.bone/agent/themes/` or `<project>/.bone/themes/`

These directories are loaded as local resources and remain subject to project trust when they are project-local.

## Internal inline runtime

Bone modules and tests may inject inline extension factories while constructing a session. The factory runtime supports Bone-owned tools, commands, event handlers, renderers, and provider registrations. It is not a filesystem loader and does not resolve package imports for external code.

## Structured UI v2

Internal extensions use `ctx.uiV2` for interactive UI. The contract is split into
product services rather than exposing renderer internals:

- `dialogs` for select, confirm, input, and notifications
- `widgets` for keyed views above or below the editor
- `chrome` for header, footer, and terminal title
- `editor` for text operations, editor dialogs, and structured editor views
- `toolResults` for structured tool call/result renderers
- `advanced` for trusted `BoneView` composition when the product services are insufficient

Factories return `BoneView`; they do not receive OpenTUI renderables. The advanced
service exposes `BoneRenderContext` and `BoneNode` through Bone's stable adapter,
not `@opentui/core` types. See `examples/extensions/ui-v2.ts`.

The v2 contract is the only extension UI surface. ANSI `Component` factories and
the former renderer-shaped dialog, editor, message, entry, and tool APIs are not
adapted or rasterized.

## Compatibility boundary

The following are intentionally unsupported:

- Pi extension packages and `package.json#pi` manifests
- Pi SDK imports and Pi package aliases
- npm/git package installation or extension update commands
- `.bone` and `~/.bone` fallback directories

Existing files are preserved silently but are not loaded.
