# @frelion/bone-tui

Bone's Bun-native OpenTUI boundary. The package exposes framework-independent `BoneView` and `BoneNode` contracts so product code does not depend directly on OpenTUI implementation types.

## Runtime

- Bun 1.3.14 or newer
- `@opentui/core` 0.4.5
- Supported OpenTUI native targets only; Termux/Android is not supported

## Renderer

```ts
import { createBoneRenderer, type BoneView } from "@frelion/bone-tui";

const view: BoneView = {
	mount(context) {
		return context.createText({ content: "Bone", bold: true });
	},
};

const renderer = await createBoneRenderer();
renderer.mount(view);
renderer.start();
```

The structured boundary includes boxes, text, Markdown, diff, raw RGBA images, scroll views, textareas, inputs, selectors, overlays, focus, keyboard and mouse events, resize handling, and deterministic test rendering.

Image decoding belongs to the product layer. `BoneImageNode` accepts raw RGBA pixels and explicit pixel and terminal dimensions.

## Testing

```ts
import { createBoneTestRenderer } from "@frelion/bone-tui";

const renderer = await createBoneTestRenderer({ width: 80, height: 24 });
renderer.start();
renderer.mount(view);
await renderer.flush();
console.log(renderer.captureFrame());
renderer.destroy();
```

The package also retains framework-independent autocomplete, fuzzy matching, terminal color report parsing, and Unicode/ANSI width utilities. The previous differential ANSI renderer, terminal abstraction, component hierarchy, editor widgets, configurable keybindings, and terminal image protocols were removed in the Bun/OpenTUI v2 migration.
