# OpenTUI Migration Plan

## Decision

Bone is a Bun-only product. The interactive terminal UI will use OpenTUI's
native renderer instead of Bone's inherited line-oriented renderer.

This is a renderer and public UI contract replacement, not a compatibility
wrapper. A migration is complete only when production interactive mode no
longer owns terminal diffing, raw input parsing, cursor placement, or overlay
composition.

## Goals

- Use `@opentui/core` as the only production terminal renderer.
- Ship Bun standalone executables with the required OpenTUI native assets.
- Represent the application as a retained renderable tree with structured
  layout, focus, input, scrolling, and selection.
- Keep Bone product concepts such as theme tokens, autocomplete, conversation
  state, and extension orchestration independent from OpenTUI implementation
  details.
- Preserve the behavior of transcript streaming, the editor, sidebars,
  dialogs, tools, images, Markdown, and session workflows.
- Replace the extension UI contract explicitly rather than silently emulating
  the old `Component.render(width): string[]` API.

## Non-goals

- Migrating the repository from npm workspaces to Bun workspaces in the same
  change.
- Introducing React or Solid as part of the renderer migration.
- Preserving source compatibility for third-party pi-tui components.
- Preserving every legacy editor/application shortcut. OpenTUI v2 has one
  documented fixed keymap; unsupported legacy shortcuts are removed from the
  product contract rather than exposed as inert bindings.
- Keeping a second production renderer after the migration is accepted.
- Rewriting agent, model, persistence, or tool business logic.

## Target Architecture

The dependency direction is:

```text
coding-agent product state and workflows
                 |
                 v
        Bone UI contracts/components
                 |
                 v
       OpenTUI renderer integration
                 |
                 v
      @opentui/core + native runtime
```

`packages/tui` owns renderer construction, lifecycle, layout primitives,
structured input adaptation, test rendering, and the small public UI contract.
It must not expose arbitrary OpenTUI internals as the default extension API.

`packages/coding-agent` owns product state and assembles the screen. It may
depend on Bone UI contracts, but product workflows must not directly manage
terminal escape sequences, framebuffer output, or native assets.

## Runtime Contract

- Source execution uses Bun.
- Official releases are Bun standalone executables.
- Interactive mode is not supported through Node.js.
- Runtime checks fail with a direct diagnostic when launched outside Bun.
- Build and test tooling may continue to use npm workspace scripts during the
  migration; package-manager conversion is a separate decision.
- Release builds must include native packages for every advertised target.
- Linux glibc and musl artifacts are built and tested independently.

## Workstreams

### 1. Renderer foundation

- Pin and review the OpenTUI dependency and native optional packages.
- Create the production renderer through a single factory.
- Create the in-memory test renderer through a matching test factory.
- Define lifecycle ownership for start, stop, suspend, resume, resize, and
  abnormal termination.
- Centralize screen mode, cursor, console interception, mouse, and terminal
  capability policy.

### 2. Bone UI boundary

- Define narrow contracts for application root, focus, overlay/dialog control,
  structured key events, theme tokens, and invalidation.
- Keep one fixed, typed Bone action map over structured key events. The v2 UI
  does not support user-configurable keybindings.
- Keep untrusted extension data behind runtime validation boundaries.
- Do not add a generic adapter that rasterizes legacy ANSI `string[]` output
  into OpenTUI text nodes.

### 3. Application shell

- Build the root flex layout.
- Add transcript, editor, footer, status tray, sidebar, and modal layers.
- Define responsive behavior for narrow and short terminals.
- Ensure fixed UI regions do not shift during streaming updates.

### 4. Conversation rendering

- Render user, assistant, thinking, tool, compaction, branch, plan, and custom
  messages as retained nodes.
- Update streaming content in place rather than rebuilding the full transcript.
- Add explicit transcript virtualization or culling for large sessions.
- Preserve copyable text without borders, line numbers, or control sequences.

### 5. Input and focus

- Move raw terminal parsing to OpenTUI structured key events.
- Map structured key events to Bone command ids through the central fixed v2
  action map; do not scatter raw key checks across components.
- Port editor behavior, autocomplete, history, multiline editing, paste, and
  submission through OpenTUI's structured textarea contract.
- Verify IME cursor placement and focus restoration after every dialog flow.

### 6. Rich content and interaction

- Port Markdown, code, diff, links, images, selection, mouse, and scrolling.
- Preserve Bone theme semantics while using structured styles.
- Verify CJK, emoji, combining characters, wide glyphs, and ANSI content from
  external tools.

### 7. Extension UI v2

- Replace renderer-shaped callbacks with product-level UI services.
- Provide explicit APIs for dialogs, widgets, headers, footers, editors,
  panels, selectors, and tool results.
- Keep a clearly named advanced escape hatch for trusted extensions that need
  custom renderables.
- Update built-in examples and reject legacy UI extensions with a useful
  migration diagnostic.
- Remove the legacy configurable-keybinding extension surface and user config
  once the legacy renderer is deleted.

### 8. Distribution and cleanup

- Embed OpenTUI native assets in every Bun executable target.
- Update local release, release doctor, metadata verification, and smoke tests.
- Remove Node runtime claims and Node interactive artifacts.
- Delete the legacy terminal driver, differential renderer, cursor marker,
  overlay compositor, and raw input parser after parity is proven.
- Remove dependencies that are no longer used after the deletion pass.

## Verification Matrix

### Rendering

- Initial screen at 80x24, 120x40, and a narrow terminal.
- Transcript growth during token streaming without layout shifts.
- Large tool output and a long session with bounded render work.
- Resize smaller and larger while streaming.
- Main screen and scrollback behavior match the chosen product policy.

### Input

- Printable text, multiline editing, paste, submit, cancel, and history.
- Ctrl, Shift, Alt/Option, function keys, repeat, and release events.
- Kitty-capable and legacy terminal input paths.
- CJK IME composition and hardware cursor position.

### Focus and overlays

- Nested dialogs, non-capturing overlays, sidebar focus, and focus restoration.
- Selector confirmation/cancellation and editor replacement.
- Foreground/background conversation switching and dialog cancellation.

### Content

- Markdown, syntax-highlighted code, diff, hyperlinks, and terminal images.
- Mouse selection and clipboard text without UI chrome.
- CJK, emoji, combining marks, regional indicators, and wide-character
  boundaries.

### Lifecycle

- Normal exit, Ctrl+C, thrown error, and signal termination.
- Terminal modes, cursor, mouse, and keyboard protocol are restored.
- Extension loading works in compiled executables.

### Release targets

- macOS arm64 and x64.
- Linux x64 and arm64 with glibc.
- Linux x64 and arm64 with musl where advertised.
- Windows x64 and arm64 where advertised.

## Required Checks

- Focused unit tests for every modified UI component or service.
- OpenTUI test-renderer frame and interaction tests for the application shell.
- Existing non-e2e suite through `./test.sh`.
- Full `npm run check` until the repository tooling is deliberately renamed.
- Bun standalone build and smoke test from outside the repository.
- Controlled terminal verification through tmux for representative workflows.
- Independent code review after implementation and before final acceptance.

## Removal Gates

The legacy renderer can be removed only when all of the following are true:

- Production startup constructs the OpenTUI renderer exclusively.
- No coding-agent source imports the legacy terminal or component contracts.
- Extension examples compile against UI v2.
- The application shell, editor, transcript, dialogs, sidebar, tools, Markdown,
  images, selection, and lifecycle scenarios pass.
- Standalone release artifacts contain the correct native library and start
  outside the repository.
- The independent review has no unresolved correctness or release blockers.

## Risks and Mitigations

- OpenTUI is pre-1.0. Pin exact versions and isolate its public surface behind
  Bone contracts.
- Native failures are harder to diagnose. Capture renderer diagnostics and
  preserve a deterministic in-memory reproduction path.
- Long transcripts still need product-level virtualization. Benchmark realistic
  sessions rather than assuming the renderer solves unbounded trees.
- Editor behavior has accumulated product semantics. Port it with behavioral
  tests instead of replacing it based on visual similarity.
- Cross-platform native assets can fail late. Make asset verification part of
  release doctor and local release smoke tests.
