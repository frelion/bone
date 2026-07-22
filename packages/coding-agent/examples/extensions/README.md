# Extension Examples

These examples demonstrate Bone's extension authoring API and structured `uiV2` services.

These files are source examples for Bone-owned inline extension factories and custom `ResourceLoader` implementations. The `bone` CLI does not discover or execute external extension files.

Extension keyboard shortcuts are intentionally unsupported. Commands are the extension entry point for user-triggered actions, and Bone's built-in keymap is fixed.

## Examples

### Lifecycle and safety

| Extension | Description |
|-----------|-------------|
| `permission-gate.ts` | Confirms potentially dangerous shell commands with `uiV2.dialogs` |
| `project-trust.ts` | Handles project trust decisions with structured select and input dialogs |
| `protected-paths.ts` | Blocks writes to protected paths |
| `confirm-destructive.ts` | Confirms destructive session actions |
| `dirty-repo-guard.ts` | Prevents session changes while the repository is dirty |
| `sandbox/` | Runs shell commands through `@anthropic-ai/sandbox-runtime` |
| `gondolin/` | Routes built-in tools into a Gondolin micro-VM |

### Tools and commands

| Extension | Description |
|-----------|-------------|
| `hello.ts` | Minimal custom tool |
| `tool-override.ts` | Wraps a built-in tool with access logging |
| `dynamic-tools.ts` | Registers tools at startup and from a command |
| `commands.ts` | Lists commands with a structured selector |
| `send-user-message.ts` | Sends, steers, and queues user messages |
| `shutdown-command.ts` | Exposes controlled shutdown through a command and tool |
| `reload-runtime.ts` | Demonstrates the safe extension reload flow |
| `ui-v2.ts` | Builds a widget with Bone views and uses structured dialogs |
| `timed-confirm.ts` | Uses dialog timeouts and `AbortSignal` cancellation |
| `rpc-demo.ts` | Exercises dialog, editor, notification, and title services over RPC |

### Session and prompt integration

| Extension | Description |
|-----------|-------------|
| `bookmark.ts` | Labels session entries for later navigation |
| `session-name.ts` | Names the current session |
| `git-checkpoint.ts` | Restores a git checkpoint when forking |
| `auto-commit-on-exit.ts` | Commits changes when a session exits |
| `git-merge-and-resolve.ts` | Fetches and merges a ref before the agent starts |
| `custom-compaction.ts` | Supplies custom compaction behavior |
| `trigger-compact.ts` | Triggers compaction from context usage or a command |
| `prompt-customizer.ts` | Adjusts the system prompt |
| `claude-rules.ts` | Loads project rules into the system prompt |
| `pirate.ts` | Toggles a small prompt customization from a command |

### System and providers

| Extension | Description |
|-----------|-------------|
| `ssh.ts` | Routes built-in tool operations to an SSH host |
| `file-trigger.ts` | Watches a file and injects its contents into the conversation |
| `event-bus.ts` | Communicates between extensions with the shared event bus |
| `notify.ts` | Sends terminal desktop notifications after agent completion |
| `inline-bash.ts` | Expands inline shell expressions in user input |
| `input-transform.ts` | Implements command-like input transformations |
| `input-transform-streaming.ts` | Skips expensive transforms for streaming input |
| `provider-payload.ts` | Observes provider request and response events |
| `custom-provider-anthropic/` | Registers a custom Anthropic provider |
| `custom-provider-gitlab-duo/` | Registers a GitLab Duo provider |
| `with-deps/` | Loads an extension with its own pinned dependencies |
| `dynamic-resources/` | Adds resources during resource discovery |

## Structured UI

Use the service that owns the interaction:

```typescript
import type { ExtensionAPI } from "@frelion/bone-coding-agent";

export default function (bone: ExtensionAPI) {
  bone.on("tool_call", async (event, ctx) => {
    if (event.toolName !== "bash") return;

    const allowed = await ctx.uiV2.dialogs.confirm({
      title: "Dangerous command",
      message: "Allow this shell command?",
    });
    if (!allowed) return { block: true, reason: "Blocked by user" };
  });

  bone.registerCommand("hello", {
    description: "Show a notification",
    handler: async (_args, ctx) => {
      ctx.uiV2.dialogs.notify("Hello", "info");
    },
  });
}
```

The v2 surface uses `dialogs`, `widgets`, `chrome`, `editor`, `toolResults`, and `advanced`. It does not expose OpenTUI renderables or legacy `Component`/`TUI` objects.
