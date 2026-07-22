# Examples

Example code for pi-coding-agent SDK and extensions.

## Directories

### [sdk/](sdk/)
Programmatic usage via `createAgentSession()`. Shows how to customize models, prompts, tools, extensions, and session management.

### [extensions/](extensions/)
Example extensions demonstrating:
- Lifecycle event handlers (tool interception, safety gates, context modifications)
- Custom tools (tool overrides, dynamic registration, and remote execution)
- Commands and fixed product keyboard behavior
- Structured UI services for dialogs, widgets, chrome, editors, and tool results
- Git integration (checkpoints, auto-commit)
- System prompt modifications and custom compaction
- External integrations (SSH, file watchers, sandboxes, and custom providers)
- Custom providers (Anthropic with custom streaming, GitLab Duo)

## Documentation

- [SDK Reference](sdk/README.md)
- [Extensions Documentation](../docs/extensions.md)
- [Skills Documentation](../docs/skills.md)
