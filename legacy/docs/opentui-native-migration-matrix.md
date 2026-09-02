# OpenTUI Native Migration Matrix

This matrix classifies the dirty worktree that existed when the native
refactor started. It separates product behavior that must survive from facade
implementation that must be removed.

| Existing file/change | Class | Native migration decision |
| --- | --- | --- |
| `opentui-selector-v2.ts` attach-then-focus patch | B | Preserve explicit attach-before-focus behavior; replace the `BoneView` implementation with native `InputRenderable`/`SelectRenderable`. |
| `opentui-settings-v2.ts` explicit form focus | B | Preserve explicit dialog focus target; remove mount-time focus and Bone node types. |
| `opentui-extension-host.ts` modal key scope and close helper | B | Preserve modal isolation and idempotent close semantics; move all key/lifecycle/focus restore work to the single native `OverlayManager`. |
| `opentui-interactive-mode.ts` presentation refresh barrier | A | Preserve `eventTail` serialization and live snapshot replay so streaming events cannot land in a destroyed transcript. Replace only renderer/view integration. |
| `packages/tui/src/opentui/renderer.ts` descendant focus restore and key scopes | B | Preserve descendant/sibling overlay regressions as native manager tests; delete `BoneRenderer`, translated key broadcast, and `getNativeNode`. |
| `packages/tui/src/opentui/types.ts` `restoreFocus`/`onKeyScope` additions | B | Delete with the rest of the parallel type facade. Native contracts use `Renderable` and `KeyEvent` directly. |
| `packages/tui/test/opentui-renderer.bun.ts` focus regressions | A/B | Retain behavior coverage using official OpenTUI `createTestRenderer`; delete the custom Bone test renderer path. |
| `docs/opentui-native-refactor-plan.md` | A | User-provided execution plan; preserve unchanged. |

No unrelated user or other-agent source changes were present in the initial
dirty diff. The refactor branch preserves all files until their native
replacement is integrated and verified; it does not reset, stash, or roll back
the starting worktree.

## File ownership

| Owner | Files |
| --- | --- |
| Main agent | `opentui-interactive-mode.ts`, `opentui-shell.ts`, pane navigator, package manifests/locks, final integration |
| Native kernel agent | `packages/tui/src/renderer.ts`, `packages/tui/src/overlay-manager.ts`, native kernel tests |
| Core UI agent | composer, sidebar, transcript scroll/factory, chrome/messages/image, built-in tool view consumers |
| Extension UI agent | Extension UI V2 contract, dialogs/selectors/settings/login, extension host, startup UI |
| Independent review agent | Read-only review after implementation and focused tests converge |
