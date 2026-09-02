# @frelion/bone-tui

Bone's Bun-native OpenTUI utilities. Product UI is built directly from `@opentui/core` renderables; this package owns only Bone-level renderer defaults, overlays, autocomplete, fuzzy matching, and terminal utilities.

## Runtime

- Bun 1.3.14 or newer
- `@opentui/core` 0.4.5
- Supported OpenTUI native targets only; Termux/Android is not supported

## Renderer

```ts
import { BoxRenderable, TextRenderable } from "@opentui/core";
import { createRenderer } from "@frelion/bone-tui";

const renderer = await createRenderer();
const root = new BoxRenderable(renderer, { width: "100%", height: "100%" });
root.add(new TextRenderable(renderer, { content: "Bone", bold: true }));
renderer.root.add(root);
renderer.start();
```

OpenTUI provides boxes, text, Markdown, diff, scroll views, textareas, inputs, selectors, focus, keyboard and mouse events, resize handling, and deterministic test rendering. `OverlayManager` is Bone's single overlay lifecycle owner.

Image decoding belongs to the product layer. Product code converts decoded image data into native OpenTUI renderables.

## Testing

```ts
import { BoxRenderable, TextRenderable } from "@opentui/core";
import { createTestRenderer } from "@opentui/core/testing";

const setup = await createTestRenderer({ width: 80, height: 24, autoFocus: false });
const root = new BoxRenderable(setup.renderer, { width: "100%", height: "100%" });
root.add(new TextRenderable(setup.renderer, { content: "Bone" }));
setup.renderer.root.add(root);
setup.renderer.start();
await setup.flush();
console.log(setup.captureCharFrame());
setup.renderer.destroy();
```

The package also retains framework-independent autocomplete, fuzzy matching, terminal color report parsing, and Unicode/ANSI width utilities. The previous differential ANSI renderer, terminal abstraction, component hierarchy, editor widgets, configurable keybindings, and terminal image protocols were removed in the Bun/OpenTUI v2 migration.
