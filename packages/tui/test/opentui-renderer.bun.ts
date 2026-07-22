import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";
import { verifyOpenTUINativeRuntime } from "../src/opentui/runtime.ts";
import { createBoneTestRenderer } from "../src/opentui/testing.ts";
import type { BoneTestRenderer, BoneTextNode } from "../src/opentui/types.ts";

const activeRenderers = new Set<BoneTestRenderer>();

async function createRenderer(width = 40, height = 12): Promise<BoneTestRenderer> {
	const renderer = await createBoneTestRenderer({ width, height });
	activeRenderers.add(renderer);
	renderer.start();
	return renderer;
}

afterEach(() => {
	for (const renderer of activeRenderers) renderer.destroy();
	activeRenderers.clear();
});

describe("OpenTUI Bone renderer", () => {
	it("verifies the native runtime through the Bone boundary", () => {
		assert.doesNotThrow(() => verifyOpenTUINativeRuntime());
	});

	it("renders root content and an overlay, then updates text in place", async () => {
		const renderer = await createRenderer();
		const text = renderer.createText({ id: "message", content: "before", fg: "#ffffff" });
		renderer.content.append(text);
		const dialog = renderer.createBox({ id: "dialog", border: true, width: 16, height: 3 });
		dialog.append(renderer.createText({ content: "overlay" }));
		const overlay = renderer.showOverlay(dialog, { captureFocus: false });

		await renderer.flush();
		assert.match(renderer.captureFrame(), /before/);
		assert.match(renderer.captureFrame(), /overlay/);

		text.content = "after";
		await renderer.flush();
		assert.match(renderer.captureFrame(), /after/);
		assert.doesNotMatch(renderer.captureFrame(), /before/);
		assert.equal(text.id, "message");

		overlay.close();
		await renderer.flush();
		assert.doesNotMatch(renderer.captureFrame(), /overlay/);
	});

	it("lays out and scrolls a transcript viewport", async () => {
		const renderer = await createRenderer(24, 6);
		const scroll = renderer.createScrollView({ width: "100%", height: "100%", scrollY: true });
		for (let index = 0; index < 12; index++) {
			scroll.append(renderer.createText({ content: `line-${index}`, height: 1 }));
		}
		renderer.content.append(scroll);

		await renderer.flush();
		scroll.scrollTo(11);
		await renderer.flush();

		assert.ok(scroll.scrollHeight >= 12);
		assert.ok(scroll.scrollTop > 0);
		assert.match(renderer.captureFrame(), /line-11/);
	});

	it("renders a flex-grown transcript beside fixed chrome", async () => {
		const renderer = await createRenderer(80, 16);
		const shell = renderer.createBox({ width: "100%", height: "100%", flexDirection: "column" });
		const body = renderer.createBox({ flexDirection: "row", flexGrow: 1, minHeight: 0 });
		const main = renderer.createBox({ flexDirection: "column", flexGrow: 1, height: "100%", minHeight: 0 });
		const transcript = renderer.createScrollView({
			flexDirection: "column",
			flexGrow: 1,
			height: "100%",
			minHeight: 0,
			scrollY: true,
			stickyScroll: true,
			stickyStart: "bottom",
		});
		transcript.append(renderer.createText({ content: "transcript" }));
		const fixed = renderer.createBox({ flexDirection: "column", flexShrink: 0 });
		fixed.append(renderer.createText({ content: "footer" }));
		main.append(transcript);
		main.append(fixed);
		body.append(main);
		shell.append(body);
		renderer.content.append(shell);

		await renderer.flush();
		assert.match(renderer.captureFrame(), /transcript/);
		assert.match(renderer.captureFrame(), /footer/);
	});

	it("renders and updates structured diffs", async () => {
		const renderer = await createRenderer(50, 8);
		const diff = renderer.createDiff({
			diff: "--- a/file.ts\n+++ b/file.ts\n@@ -1 +1 @@\n-old\n+new",
			view: "unified",
			showLineNumbers: false,
		});
		renderer.content.append(diff);
		await renderer.flush();
		assert.match(renderer.captureFrame(), /old/);
		assert.match(renderer.captureFrame(), /new/);

		diff.diff = "--- a/file.ts\n+++ b/file.ts\n@@ -1 +1 @@\n-before\n+after";
		await renderer.flush();
		assert.match(renderer.captureFrame(), /after/);
	});

	it("renders validated RGBA images through the OpenTUI framebuffer", async () => {
		const renderer = await createRenderer(12, 6);
		const pixels = new Uint8Array(4 * 4 * 4).fill(255);
		const image = renderer.createImage({
			pixels,
			pixelWidth: 4,
			pixelHeight: 4,
			terminalWidth: 4,
			terminalHeight: 2,
		});
		renderer.content.append(image);
		await renderer.flush();
		assert.equal(image.pixelWidth, 4);
		assert.equal(image.pixelHeight, 4);
		assert.throws(() => image.setPixels(new Uint8Array(3), 2, 2), /Expected 16 RGBA bytes/);
	});

	it("accepts textarea input, paste, and submit through structured input", async () => {
		const renderer = await createRenderer();
		let changed = "";
		let submitted = "";
		const textarea = renderer.createTextarea({
			id: "prompt",
			height: 3,
			onChange: (value) => {
				changed = value;
			},
			onSubmit: (value) => {
				submitted = value;
			},
		});
		renderer.content.append(textarea);
		renderer.focus(textarea);

		await renderer.input.typeText("hello");
		await renderer.input.paste(" world");
		await renderer.flush();
		assert.equal(textarea.value, "hello world");
		assert.equal(changed, "hello world");

		renderer.input.pressEnter({ meta: true });
		await renderer.flush();
		assert.equal(submitted, "hello world");
		assert.ok(renderer.captureCursor().x >= 0);
	});

	it("supports single-line input events, limits, cancellation, and custom keybindings", async () => {
		const renderer = await createRenderer();
		const inputs: string[] = [];
		let changed = "";
		let confirmed = "";
		let cancelled = 0;
		const input = renderer.createInput({
			id: "search",
			placeholder: "Search",
			maxLength: 8,
			keyBindings: [
				{ name: "s", ctrl: true, action: "submit" },
				{ name: "q", action: "cancel" },
			],
			onInput: (value) => inputs.push(value),
			onChange: (value) => {
				changed = value;
			},
			onConfirm: (value) => {
				confirmed = value;
			},
			onCancel: () => {
				cancelled++;
			},
		});
		renderer.content.append(input);
		renderer.focus(input);

		await renderer.input.typeText("hello");
		await renderer.input.paste("\nworld");
		renderer.input.pressKey("s", { ctrl: true });
		await renderer.flush();

		assert.equal(input.value, "hellowor");
		assert.equal(input.placeholder, "Search");
		assert.equal(input.maxLength, 8);
		assert.equal(inputs.at(-1), "hellowor");
		assert.equal(changed, "hellowor");
		assert.equal(confirmed, "hellowor");

		renderer.input.pressKey("q");
		await renderer.flush();
		assert.equal(cancelled, 1);
	});

	it("supports typed select items, selection events, confirmation, cancellation, and item updates", async () => {
		const renderer = await createRenderer();
		const changes: number[] = [];
		let confirmed = "";
		let cancelled = 0;
		const select = renderer.createSelect({
			height: 5,
			showDescription: false,
			items: [
				{ label: "Alpha", value: { id: "a" } },
				{ label: "Beta", value: { id: "b" } },
				{ label: "Gamma", value: { id: "c" } },
			],
			keyBindings: [
				{ name: "n", ctrl: true, action: "move-down" },
				{ name: "q", action: "cancel" },
			],
			onChange: (_item, index) => changes.push(index),
			onConfirm: (item) => {
				confirmed = item.value.id;
			},
			onCancel: () => {
				cancelled++;
			},
		});
		renderer.content.append(select);
		renderer.focus(select);

		renderer.input.pressKey("n", { ctrl: true });
		renderer.input.pressEnter();
		await renderer.flush();
		assert.equal(select.selectedIndex, 1);
		assert.equal(select.selectedItem?.value.id, "b");
		assert.deepEqual(changes, [1]);
		assert.equal(confirmed, "b");
		assert.match(renderer.captureFrame(), /Beta/);

		select.items = [{ label: "Delta", value: { id: "d" } }];
		assert.equal(select.selectedItem?.value.id, "d");
		renderer.input.pressKey("q");
		await renderer.flush();
		assert.equal(cancelled, 1);
	});

	it("provides structured mouse down and scroll events", async () => {
		const renderer = await createRenderer(30, 8);
		let clicked = false;
		let direction = "";
		const target = renderer.createText({
			content: "mouse target",
			width: 20,
			height: 4,
			onMouseDown: (event) => {
				clicked = event.x >= 0 && event.y >= 0 && event.button === 0;
			},
			onMouseScroll: (event) => {
				direction = event.scrollDirection ?? "";
				event.preventDefault();
			},
		});
		renderer.content.append(target);
		await renderer.flush();

		await renderer.mouse.click(2, 1);
		await renderer.mouse.scroll(2, 1, "down");
		assert.equal(clicked, true);
		assert.equal(direction, "down");
	});

	it("restores focus when an overlay closes", async () => {
		const renderer = await createRenderer();
		const base = renderer.createTextarea({ id: "base", height: 2 });
		const modal = renderer.createTextarea({ id: "modal", height: 2, width: 20 });
		renderer.content.append(base);
		renderer.focus(base);
		const overlay = renderer.showOverlay(modal);

		await renderer.input.typeText("modal");
		assert.equal(modal.value, "modal");
		overlay.close();
		await renderer.input.typeText("base");
		await renderer.flush();

		assert.equal(modal.destroyed, true);
		assert.equal(base.value, "base");
	});

	it("supports resize, key unsubscribe, clear, and destroy", async () => {
		const renderer = await createRenderer();
		let keyEvents = 0;
		let resized: { width: number; height: number } | undefined;
		const unsubscribe = renderer.onKey(() => {
			keyEvents++;
		});
		const unsubscribeResize = renderer.onResize((width, height) => {
			resized = { width, height };
		});
		const text: BoneTextNode = renderer.createText({ content: "wide" });
		renderer.content.append(text);
		renderer.input.pressKey("a");
		unsubscribe();
		renderer.input.pressKey("b");
		renderer.resize(28, 8);
		await renderer.flush();

		assert.equal(keyEvents, 1);
		assert.equal(renderer.width, 28);
		assert.equal(renderer.height, 8);
		assert.deepEqual(resized, { width: 28, height: 8 });
		unsubscribeResize();
		renderer.content.clear();
		assert.equal(renderer.content.childCount, 0);
		assert.equal(text.destroyed, true);

		renderer.destroy();
		activeRenderers.delete(renderer);
		assert.equal(renderer.root.destroyed, true);
	});
});
