# OpenTUI Native Runtime Contract

This document records the Phase 0 runtime behavior verified against
`@opentui/core@0.4.5`. Product components must follow these constraints during
the native migration.

## Construction and ownership

- Construct renderables directly with a `CliRenderer` as their `RenderContext`.
- Attach trees with `parent.add(child)` and destroy owned subtrees with
  `destroyRecursively()`.
- `CliRenderer` is the only renderer and native focus owner. Bone must not mirror
  `currentFocusedRenderable` in a second focus state.
- Production and test renderers use `autoFocus: false`. Pane and overlay owners
  explicitly focus a control only after its tree is attached.
- OpenTUI permits focus before attachment; that behavior is not a valid product
  lifecycle and must not be used by component constructors or factories.

## Focus lifecycle

Observed focus transition from control A to control B:

1. A emits `blurred`.
2. `CliRenderer` emits `FOCUSED_RENDERABLE` with B and A.
3. B emits `focused`.

Destroying a focused renderable clears `currentFocusedRenderable`, emits the
renderer focus transition to `null`, then emits the control's blur and destroy
events. Overlay close must still blur the focused descendant before destroying
the subtree so focus restoration is explicit and deterministic.

## Input routing

- Global `keyInput` listeners run before focused-renderable internal handlers.
- Global application routing may stop propagation only for recognized product
  actions such as pane navigation, interrupt, quit, or conversation switching.
- Ordinary text, cursor editing, paste, textarea submit/cancel, select movement,
  and ScrollBox navigation belong to the focused native control.
- Key events must never be translated and broadcast to every pane.

## Layout, scrolling, and testing

- Native renderable layout properties are the only layout state.
- Each transcript and sidebar owns exactly one `ScrollBoxRenderable`; its native
  `scrollTop`, `scrollHeight`, `scrollBy`, `scrollTo`, mouse routing, and sticky
  state are authoritative.
- Native tests use `createTestRenderer`, `mockInput`, `mockMouse`, `flush`,
  `resize`, and frame capture from `@opentui/core/testing` directly.

## Native coverage exception

OpenTUI 0.4.5 provides no `ImageRenderable`. Bone may keep a product-specific
RGBA image renderable subclassing native `FrameBufferRenderable`; it must not
reintroduce a generic node, renderer, layout, focus, event, or lifecycle facade.
