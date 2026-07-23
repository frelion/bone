import assert from "node:assert/strict";
import { afterEach, describe, it } from "node:test";
import {
	BoxRenderable,
	DiffRenderable,
	FrameBufferRenderable,
	InputRenderable,
	MarkdownRenderable,
	type RenderContext,
	ScrollBoxRenderable,
	SelectRenderable,
	SelectRenderableEvents,
	SyntaxStyle,
	TextareaRenderable,
	TextRenderable,
} from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { type OverlayDescriptor, OverlayManager, OverlayOpenCancelledError } from "../src/overlay-manager.ts";

type OverlayDescriptorForTest = OverlayDescriptor<BoxRenderable>;

declare const Bun: {
	FFI: { ptr(value: ArrayBufferView): number };
};

class TestImageRenderable extends FrameBufferRenderable {
	constructor(context: RenderContext, pixels: Uint8Array) {
		super(context, { width: 2, height: 1 });
		this.frameBuffer.drawSuperSampleBuffer(0, 0, Bun.FFI.ptr(pixels), pixels.length, "rgba8unorm", 8);
	}
}

const activeSetups = new Set<TestRendererSetup>();

async function createSetup(width = 60, height = 16): Promise<TestRendererSetup> {
	const setup = await createTestRenderer({ width, height, autoFocus: false, useMouse: true });
	activeSetups.add(setup);
	setup.renderer.start();
	return setup;
}

afterEach(() => {
	for (const setup of activeSetups) setup.renderer.destroy();
	activeSetups.clear();
});

describe("native TUI kernel", () => {
	it("renders native text editing, select, markdown, diff, and ScrollBox controls", async () => {
		const { renderer, mockInput, mockMouse, flush, captureCharFrame } = await createSetup();
		const shell = new BoxRenderable(renderer, { width: "100%", height: "100%", flexDirection: "column" });
		const transcript = new ScrollBoxRenderable(renderer, {
			id: "transcript",
			flexGrow: 1,
			minHeight: 0,
			scrollY: true,
			stickyScroll: false,
		});
		const markdown = new MarkdownRenderable(renderer, {
			content: "# Native\n\nstreaming body",
			syntaxStyle: SyntaxStyle.fromStyles({ default: { fg: "#ffffff" } }),
			streaming: true,
		});
		const diff = new DiffRenderable(renderer, {
			diff: "--- a/file.ts\n+++ b/file.ts\n@@ -1 +1 @@\n-old\n+new",
			view: "unified",
			showLineNumbers: false,
		});
		const image = new TestImageRenderable(renderer, new Uint8Array(16).fill(255));
		transcript.add(markdown);
		transcript.add(diff);
		transcript.add(image);
		for (let index = 0; index < 12; index++)
			transcript.add(new TextRenderable(renderer, { content: `line-${index}`, height: 1 }));
		const textarea = new TextareaRenderable(renderer, { id: "textarea", height: 2, width: "100%" });
		let selected = -1;
		const select = new SelectRenderable(renderer, {
			id: "select",
			height: 3,
			showDescription: false,
			options: [
				{ name: "one", description: "", value: 1 },
				{ name: "two", description: "", value: 2 },
			],
		});
		select.on(SelectRenderableEvents.SELECTION_CHANGED, (index: number) => {
			selected = index;
		});
		shell.add(transcript);
		shell.add(textarea);
		shell.add(select);
		renderer.root.add(shell);
		textarea.focus();
		await mockInput.typeText("hello");
		await mockInput.pasteBracketedText(" world");
		transcript.scrollTo(0);
		await flush();
		assert.equal(textarea.plainText, "hello world");
		assert.equal(image.width, 2);
		assert.match(captureCharFrame(), /Native/);
		assert.match(captureCharFrame(), /new/);

		select.focus();
		mockInput.pressArrow("down");
		await flush();
		assert.equal(select.getSelectedIndex(), 1);
		assert.equal(selected, 1);
		assert.ok(transcript.scrollHeight > transcript.height);
		await mockMouse.scroll(2, 2, "up");
		assert.ok(transcript.scrollTop >= 0);
	});

	it("owns modal focus and close lifecycle in one manager", async () => {
		const { renderer, mockInput, flush, resize } = await createSetup(40, 12);
		const composer = new InputRenderable(renderer, { id: "composer", width: 30 });
		renderer.root.add(composer);
		composer.focus();
		const manager = new OverlayManager(renderer);
		const firstRoot = new BoxRenderable(renderer, { id: "first", width: 24, height: 4 });
		const firstControl = new InputRenderable(renderer, { id: "first-control", width: 20 });
		firstRoot.add(firstControl);
		let modalActions = 0;
		const first = manager.open(
			{ root: firstRoot, focusTarget: firstControl },
			{
				restoreFocus: composer,
				layout: { anchor: "top-center", width: 24, height: 4 },
				onKey: (event) => {
					if (event.name !== "f1") return false;
					modalActions++;
					return true;
				},
			},
		);
		mockInput.pressKey("F1");
		await mockInput.typeText("modal");
		assert.equal(modalActions, 1);
		assert.equal(firstControl.value, "modal");
		first.updateLayout({ anchor: "bottom-right", width: 28 });
		resize(56, 18);
		await flush();
		await first.close();
		await mockInput.typeText("ready");
		assert.equal(composer.value, "ready");

		const abortController = new AbortController();
		const secondRoot = new BoxRenderable(renderer, { id: "second", width: 20, height: 3 });
		const secondControl = new InputRenderable(renderer, { id: "second-control", width: 18 });
		secondRoot.add(secondControl);
		const second = manager.open(
			{ root: secondRoot, focusTarget: secondControl },
			{
				restoreFocus: composer,
				signal: abortController.signal,
			},
		);
		const closeAgain = second.close("close");
		abortController.abort();
		await Promise.all([closeAgain, second.close("abort")]);
		assert.equal(second.state, "closed");
		assert.equal(manager.size, 0);
		await manager.dispose();
	});

	it("does not steal focus from a newer sibling overlay", async () => {
		const { renderer, mockInput } = await createSetup();
		const composer = new InputRenderable(renderer, { id: "composer-sibling", width: 30 });
		renderer.root.add(composer);
		composer.focus();
		const manager = new OverlayManager(renderer);
		const firstRoot = new BoxRenderable(renderer, { id: "first-sibling", width: 20, height: 3 });
		const first = manager.open({ root: firstRoot }, { restoreFocus: composer });
		const secondRoot = new BoxRenderable(renderer, { id: "second-sibling", width: 20, height: 3 });
		const secondControl = new InputRenderable(renderer, { id: "second-sibling-control", width: 18 });
		secondRoot.add(secondControl);
		const second = manager.open({ root: secondRoot, focusTarget: secondControl }, { restoreFocus: composer });
		await first.close();
		await mockInput.typeText("still-modal");
		assert.equal(secondControl.value, "still-modal");
		await second.close();

		const olderRoot = new BoxRenderable(renderer, { id: "older-unfocused", width: 20, height: 3 });
		const older = manager.open({ root: olderRoot }, { restoreFocus: composer });
		const newerRoot = new BoxRenderable(renderer, { id: "newer-unfocused", width: 20, height: 3 });
		const newer = manager.open({ root: newerRoot }, { restoreFocus: composer });
		composer.blur();
		await older.close();
		assert.equal(renderer.currentFocusedRenderable, newer.wrapper);
		await newer.close();
		assert.equal(renderer.currentFocusedRenderable, composer);
		await manager.dispose();
	});

	it("cancels async factories before attach and destroys late results", async () => {
		const { renderer } = await createSetup();
		const composer = new InputRenderable(renderer, { id: "composer-async", width: 30 });
		renderer.root.add(composer);
		composer.focus();
		const manager = new OverlayManager(renderer);
		const preAbortedController = new AbortController();
		preAbortedController.abort();
		let preAbortedFactoryCalled = false;
		await assert.rejects(
			manager.openAsync(
				() => {
					preAbortedFactoryCalled = true;
					return { root: new BoxRenderable(renderer, { width: 20, height: 3 }) };
				},
				{ restoreFocus: composer, signal: preAbortedController.signal },
			),
			(error: unknown) => error instanceof OverlayOpenCancelledError && error.reason === "abort",
		);
		assert.equal(preAbortedFactoryCalled, false);
		const controller = new AbortController();
		let factoryCalled = false;
		let resolveFactory: (descriptor: OverlayDescriptorForTest) => void = () => undefined;
		const factory = new Promise<OverlayDescriptorForTest>((resolve) => {
			resolveFactory = resolve;
		});
		const opening = manager.openAsync(
			() => {
				factoryCalled = true;
				return factory;
			},
			{ restoreFocus: composer, signal: controller.signal },
		);
		assert.equal(manager.size, 1);
		await Promise.resolve();
		assert.equal(factoryCalled, true);
		controller.abort();
		await assert.rejects(
			opening,
			(error: unknown) => error instanceof OverlayOpenCancelledError && error.reason === "abort",
		);
		const sidebar = new InputRenderable(renderer, { id: "sidebar-after-cancel", width: 20 });
		renderer.root.add(sidebar);
		sidebar.focus();
		const lateRoot = new BoxRenderable(renderer, { id: "late-root", width: 20, height: 3 });
		resolveFactory({ root: lateRoot });
		await Promise.resolve();
		await Promise.resolve();
		assert.equal(lateRoot.isDestroyed, true);
		assert.equal(manager.size, 0);
		assert.equal(renderer.currentFocusedRenderable, sidebar);

		let resolveDisposedFactory: (descriptor: OverlayDescriptorForTest) => void = () => undefined;
		const disposedFactory = new Promise<OverlayDescriptorForTest>((resolve) => {
			resolveDisposedFactory = resolve;
		});
		const disposedOpening = manager.openAsync(() => disposedFactory, { restoreFocus: composer });
		await Promise.resolve();
		const disposing = manager.dispose();
		await assert.rejects(
			disposedOpening,
			(error: unknown) => error instanceof OverlayOpenCancelledError && error.reason === "dispose",
		);
		await disposing;
		const disposedLateRoot = new BoxRenderable(renderer, { id: "disposed-late-root", width: 20, height: 3 });
		resolveDisposedFactory({ root: disposedLateRoot });
		await Promise.resolve();
		await Promise.resolve();
		assert.equal(disposedLateRoot.isDestroyed, true);
	});

	it("handles factory errors, timeout, async close, resize, and repeated focus restore", async () => {
		const { renderer, resize, flush } = await createSetup();
		const composer = new InputRenderable(renderer, { id: "composer-repeated", width: 30 });
		renderer.root.add(composer);
		composer.focus();
		const manager = new OverlayManager(renderer);
		const invalidRoot = new BoxRenderable(renderer, { id: "invalid-timeout", width: 20, height: 3 });
		assert.throws(
			() => manager.open({ root: invalidRoot }, { restoreFocus: composer, timeoutMs: -1 }),
			/finite non-negative/,
		);
		assert.equal(invalidRoot.parent, null);
		assert.equal(manager.layer.getChildrenCount(), 0);
		let invalidFactoryCalled = false;
		await assert.rejects(
			manager.openAsync(
				() => {
					invalidFactoryCalled = true;
					return { root: new BoxRenderable(renderer, { width: 20, height: 3 }) };
				},
				{ restoreFocus: composer, timeoutMs: Number.NaN },
			),
			/finite non-negative/,
		);
		assert.equal(invalidFactoryCalled, false);
		await assert.rejects(
			manager.openAsync(
				() => {
					const root = new BoxRenderable(renderer, { width: 20, height: 3 });
					const control = new InputRenderable(renderer, { width: 18 });
					root.add(control);
					control.focus();
					return { root, focusTarget: control };
				},
				{ restoreFocus: composer },
			),
			/must not focus.*before native tree attachment/,
		);
		assert.equal(renderer.currentFocusedRenderable, composer);
		await assert.rejects(
			manager.openAsync(
				() => {
					throw new Error("sync factory failure");
				},
				{ restoreFocus: composer },
			),
			/sync factory failure/,
		);
		await assert.rejects(
			manager.openAsync(
				async () => {
					throw new Error("async factory failure");
				},
				{ restoreFocus: composer },
			),
			/async factory failure/,
		);
		await assert.rejects(
			manager.openAsync(() => undefined as never, { restoreFocus: composer }),
			/returned no descriptor/,
		);
		assert.equal(manager.size, 0);
		const timeoutController = new AbortController();
		const timeoutOpening = manager.openAsync(() => new Promise<OverlayDescriptorForTest>(() => undefined), {
			restoreFocus: composer,
			signal: timeoutController.signal,
			timeoutMs: 1,
		});
		await assert.rejects(
			timeoutOpening,
			(error: unknown) => error instanceof OverlayOpenCancelledError && error.reason === "timeout",
		);

		let releaseClose: () => void = () => undefined;
		const closeDone = new Promise<void>((resolve) => {
			releaseClose = resolve;
		});
		for (let index = 0; index < 10; index++) {
			const root = new BoxRenderable(renderer, { id: `repeat-${index}`, width: 20, height: 3 });
			const control = new InputRenderable(renderer, { id: `repeat-control-${index}`, width: 18 });
			root.add(control);
			const handle = manager.open({ root, focusTarget: control }, { restoreFocus: composer });
			assert.equal(renderer.currentFocusedRenderable, control);
			await handle.close();
			assert.equal(renderer.currentFocusedRenderable, composer);
		}
		const closingRoot = new BoxRenderable(renderer, { id: "closing-root", width: 20, height: 3 });
		const closingControl = new InputRenderable(renderer, { id: "closing-control", width: 18 });
		closingRoot.add(closingControl);
		const closing = manager.open(
			{ root: closingRoot, focusTarget: closingControl },
			{
				restoreFocus: composer,
				onClose: () => closeDone,
			},
		);
		const firstClose = closing.close();
		const secondClose = closing.close("abort");
		assert.equal(firstClose, secondClose);
		resize(72, 20);
		await flush();
		releaseClose();
		await firstClose;
		assert.equal(closingRoot.isDestroyed, true);
		assert.equal(renderer.currentFocusedRenderable, composer);
		await manager.dispose();
	});
});
