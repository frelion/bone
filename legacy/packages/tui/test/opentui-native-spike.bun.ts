import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";
import {
	BoxRenderable,
	CliRenderEvents,
	InputRenderable,
	RenderableEvents,
	ScrollBoxRenderable,
	TextRenderable,
} from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";

const activeSetups = new Set<TestRendererSetup>();

async function createSetup(width = 40, height = 12): Promise<TestRendererSetup> {
	const setup = await createTestRenderer({ width, height, autoFocus: false, useMouse: true });
	activeSetups.add(setup);
	setup.renderer.start();
	return setup;
}

afterEach(() => {
	for (const setup of activeSetups) setup.renderer.destroy();
	activeSetups.clear();
});

describe("OpenTUI 0.4.5 native spike", () => {
	it("documents focus ordering and clears native focus when a focused control is destroyed", async () => {
		const { renderer, mockInput, flush } = await createSetup();
		const first = new InputRenderable(renderer, { id: "first", width: 20 });
		const second = new InputRenderable(renderer, { id: "second", width: 20 });
		const events: string[] = [];

		first.on(RenderableEvents.FOCUSED, () => events.push("first:focused"));
		first.on(RenderableEvents.BLURRED, () => events.push("first:blurred"));
		second.on(RenderableEvents.FOCUSED, () => events.push("second:focused"));
		second.on(RenderableEvents.BLURRED, () => events.push("second:blurred"));
		second.on(RenderableEvents.DESTROYED, () => events.push("second:destroyed"));
		renderer.on(CliRenderEvents.FOCUSED_RENDERABLE, (current, previous) => {
			events.push(`renderer:${current?.id ?? "none"}<-${previous?.id ?? "none"}`);
		});

		// OpenTUI permits focus before attachment, so application code must enforce attach-before-focus.
		first.focus();
		assert.equal(renderer.currentFocusedRenderable, first);
		renderer.root.add(first);
		renderer.root.add(second);
		second.focus();
		await mockInput.typeText("native");
		assert.equal(second.value, "native");

		second.destroyRecursively();
		await flush();
		assert.equal(renderer.currentFocusedRenderable, null);
		assert.deepEqual(events, [
			"renderer:first<-none",
			"first:focused",
			"first:blurred",
			"renderer:second<-first",
			"second:focused",
			"renderer:none<-second",
			"second:blurred",
			"second:destroyed",
		]);
	});

	it("supports native overlay attach, explicit focus restore, resize, scrolling, and mock input", async () => {
		const { renderer, mockInput, captureCharFrame, flush, resize } = await createSetup(32, 8);
		const application = new BoxRenderable(renderer, {
			id: "application",
			width: "100%",
			height: "100%",
			flexDirection: "column",
		});
		const transcript = new ScrollBoxRenderable(renderer, {
			id: "transcript",
			width: "100%",
			flexGrow: 1,
			minHeight: 0,
			scrollY: true,
			stickyScroll: true,
			stickyStart: "bottom",
		});
		for (let index = 0; index < 12; index++) {
			transcript.add(new TextRenderable(renderer, { content: `line-${index}`, height: 1 }));
		}
		const composer = new InputRenderable(renderer, { id: "composer", width: "100%" });
		application.add(transcript);
		application.add(composer);
		renderer.root.add(application);
		composer.focus();

		const overlayLayer = new BoxRenderable(renderer, {
			id: "overlays",
			position: "absolute",
			top: 0,
			right: 0,
			bottom: 0,
			left: 0,
			zIndex: 10_000,
			alignItems: "center",
			justifyContent: "center",
		});
		const dialog = new BoxRenderable(renderer, { id: "dialog", width: 20, height: 3, border: true });
		const dialogInput = new InputRenderable(renderer, { id: "dialog-input", width: 18 });
		dialog.add(dialogInput);
		overlayLayer.add(dialog);
		renderer.root.add(overlayLayer);
		dialogInput.focus();
		await mockInput.typeText("dialog");
		assert.equal(dialogInput.value, "dialog");

		dialogInput.blur();
		overlayLayer.destroyRecursively();
		composer.focus();
		await mockInput.typeText("ready");
		transcript.scrollTo(0);
		resize(48, 10);
		await flush();

		assert.equal(composer.value, "ready");
		assert.equal(renderer.width, 48);
		assert.equal(renderer.height, 10);
		assert.equal(renderer.currentFocusedRenderable, composer);
		assert.equal(overlayLayer.isDestroyed, true);
		assert.match(captureCharFrame(), /ready/);
		assert.ok(transcript.scrollHeight >= 12);
	});
});
