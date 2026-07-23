import { OverlayManager } from "@frelion/bone-tui";
import { BoxRenderable, TextareaRenderable, TextRenderable } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { ExtensionUIView } from "../src/core/extensions/ui-v2.ts";
import type { ReadonlyFooterDataProvider } from "../src/core/footer-data-provider.ts";
import { OpenTUIExtensionHost } from "../src/modes/interactive/opentui-extension-host.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const setups = new Set<TestRendererSetup>();
const managers = new Set<OverlayManager>();
const setupByRenderer = new WeakMap<TestRendererSetup["renderer"], TestRendererSetup>();

function textView(content: string) {
	return (renderer: TestRendererSetup["renderer"]) => new TextRenderable(renderer, { content });
}

async function flushUntil(renderer: TestRendererSetup["renderer"], expected: string): Promise<string> {
	const setup = setupByRenderer.get(renderer);
	if (!setup) throw new Error("Missing OpenTUI test renderer setup");
	for (let attempt = 0; attempt < 8; attempt++) {
		await setup.flush();
		const frame = setup.captureCharFrame();
		if (frame.includes(expected)) return frame;
	}
	return setup.captureCharFrame();
}

async function setup() {
	const testSetup = await createTestRenderer({ width: 80, height: 28, autoFocus: false, useMouse: true });
	setups.add(testSetup);
	const { renderer } = testSetup;
	setupByRenderer.set(renderer, testSetup);
	const root = new BoxRenderable(renderer, { width: "100%", height: "100%", flexDirection: "column" });
	const region = (): BoxRenderable => new BoxRenderable(renderer, { width: "100%", flexDirection: "column" });
	const regions = {
		header: region(),
		aboveEditor: region(),
		editor: region(),
		belowEditor: region(),
		footer: region(),
	};
	for (const child of Object.values(regions)) root.add(child);
	renderer.root.add(root);
	const overlayManager = new OverlayManager(renderer);
	let editorText = "seed";
	let branchCallback: (() => void) | undefined;
	const footerData: ReadonlyFooterDataProvider = {
		getGitBranch: () => "main",
		getExtensionStatuses: () => new Map(),
		getAvailableProviderCount: () => 2,
		onBranchChange: (callback) => {
			branchCallback = callback;
			return () => {
				branchCallback = undefined;
			};
		},
	};
	const notify = vi.fn();
	const title = vi.fn();
	const host = new OpenTUIExtensionHost({
		renderer,
		overlayManager,
		regions,
		footerData,
		editor: {
			getText: () => editorText,
			setText: (text) => {
				editorText = text;
			},
			insertText: (text) => {
				editorText += text;
			},
		},
		onNotify: notify,
		onTitle: title,
	});
	renderer.start();
	managers.add(overlayManager);
	return {
		setup: testSetup,
		input: testSetup.mockInput,
		renderer,
		regions,
		host,
		notify,
		title,
		getEditorText: () => editorText,
		refreshBranch: () => branchCallback?.(),
	};
}

beforeEach(() => initTheme("dark"));

afterEach(async () => {
	for (const manager of managers) await manager.dispose();
	managers.clear();
	for (const setup of setups) setup.renderer.destroy();
	setups.clear();
});

describe("OpenTUIExtensionHost", () => {
	test("runs select, confirm, and input dialogs with the fixed keymap", async () => {
		const { renderer, host, input } = await setup();
		const selected = host.context.dialogs.select({
			title: "Choose provider",
			options: [
				{ value: "a", label: "Alpha", disabled: true },
				{ value: "b", label: "Beta" },
			],
		});
		expect(await flushUntil(renderer, "Choose provider")).toContain("Beta");
		input.pressArrow("down");
		input.pressEnter();
		await setupByRenderer.get(renderer)?.flush();
		await expect(selected).resolves.toBe("b");

		const confirmed = host.context.dialogs.confirm({ title: "Remove", message: "Remove this item?" });
		expect(await flushUntil(renderer, "Remove this item?")).toContain("Confirm");
		input.pressArrow("down");
		input.pressEnter();
		await expect(confirmed).resolves.toBe(false);

		const dialogInput = host.context.dialogs.input({ title: "Name", initialValue: "bo" });
		await flushUntil(renderer, "Name");
		await input.typeText("ne");
		input.pressEnter();
		await expect(dialogInput).resolves.toBe("bone");
	});

	test("cancels dialogs on escape, abort, and timeout", async () => {
		const { renderer, host, input } = await setup();
		const escaped = host.context.dialogs.input({ title: "Escape me" });
		await flushUntil(renderer, "Escape me");
		input.pressEscape();
		await expect(escaped).resolves.toBeUndefined();

		const controller = new AbortController();
		const aborted = host.context.dialogs.select({
			title: "Abort me",
			options: [{ value: "x", label: "X" }],
			signal: controller.signal,
		});
		controller.abort();
		await expect(aborted).resolves.toBeUndefined();

		await expect(host.context.dialogs.input({ title: "Timeout", timeoutMs: 0 })).resolves.toBeUndefined();
	});

	test("orders, updates, and clears widgets while managing chrome and editor views", async () => {
		const { renderer, host, notify, title, getEditorText, refreshBranch } = await setup();
		const late = host.context.widgets.set("late", textView("widget-late"), { order: 20 });
		host.context.widgets.set("early", textView("widget-early"), { order: 10 });
		let frame = await flushUntil(renderer, "widget-late");
		expect(frame.indexOf("widget-early")).toBeLessThan(frame.indexOf("widget-late"));
		late.update(textView("widget-updated"));
		expect(await flushUntil(renderer, "widget-updated")).not.toContain("widget-late");
		host.context.widgets.clear("late");
		await setupByRenderer.get(renderer)?.flush();
		expect(setupByRenderer.get(renderer)?.captureCharFrame()).not.toContain("widget-updated");

		const header = host.context.chrome.setHeader(textView("custom-header"));
		host.context.chrome.setFooter((data) => textView(`branch:${data.getGitBranch()}`));
		host.context.editor.setView(textView("custom-editor"));
		frame = await flushUntil(renderer, "custom-editor");
		expect(frame).toContain("custom-header");
		expect(frame).toContain("branch:main");
		expect(header.mounted).toBe(true);
		refreshBranch();
		await setupByRenderer.get(renderer)?.flush();

		host.context.editor.setText("hello");
		host.context.editor.insertText(" world");
		expect(host.context.editor.getText()).toBe("hello world");
		expect(getEditorText()).toBe("hello world");
		host.context.chrome.setTitle("Bone task");
		host.context.dialogs.notify("Saved", "info");
		expect(title).toHaveBeenCalledWith("Bone task");
		expect(notify).toHaveBeenCalledWith("Saved", "info");
	});

	test("registers tool renderers and resolves or closes advanced views", async () => {
		const { renderer, host } = await setup();
		const toolRenderer = { renderCall: () => textView("tool-call") };
		host.context.toolResults.setRenderer("read", toolRenderer);
		expect(host.getToolRenderer("read")).toBe(toolRenderer);
		host.context.toolResults.setRenderer("read", undefined);
		expect(host.getToolRenderer("read")).toBeUndefined();

		let finish: ((value: string) => void) | undefined;
		const advanced = host.context.advanced.show<string>((control) => {
			finish = control.done;
			return textView("advanced-view");
		});
		expect(await flushUntil(renderer, "advanced-view")).toContain("advanced-view");
		finish?.("done");
		await expect(advanced).resolves.toBe("done");

		const closed = host.context.advanced.show(() => textView("close-me"));
		await flushUntil(renderer, "close-me");
		host.context.advanced.close();
		await expect(closed).resolves.toBeUndefined();
	});

	test("focuses the first native input inside an advanced view wrapper", async () => {
		const { renderer, host } = await setup();
		let finish: (() => void) | undefined;
		let input: TextareaRenderable | undefined;
		const advanced = host.context.advanced.show<void>((control) => {
			finish = control.cancel;
			const wrapper = new BoxRenderable(renderer, { width: 30, height: 3 });
			input = new TextareaRenderable(renderer, { width: 28, height: 1, initialValue: "editable" });
			wrapper.add(input);
			return wrapper;
		});
		await flushUntil(renderer, "editable");
		expect(input?.focused).toBe(true);
		finish?.();
		await expect(advanced).resolves.toBeUndefined();
	});

	test("invalidates the retained context and settles active UI when disposed", async () => {
		const { renderer, host, notify, title, getEditorText } = await setup();
		const context = host.context;
		const widget = context.widgets.set("status", textView("active-widget"));
		context.chrome.setHeader(textView("active-header"));
		context.editor.setView(textView("active-editor"));
		const dialog = context.dialogs.input({ title: "Pending input" });
		await flushUntil(renderer, "Pending input");

		host.dispose();

		expect(context.available).toBe(false);
		await expect(dialog).resolves.toBeUndefined();
		expect(widget.mounted).toBe(false);
		widget.update(textView("must-not-remount"));
		await setupByRenderer.get(renderer)?.flush();
		const frame = setupByRenderer.get(renderer)?.captureCharFrame() ?? "";
		expect(frame).not.toContain("Pending input");
		expect(frame).not.toContain("active-widget");
		expect(frame).not.toContain("active-header");
		expect(frame).not.toContain("active-editor");
		expect(frame).not.toContain("must-not-remount");

		const requestRender = vi.spyOn(renderer, "requestRender");
		const afterDisposeWidget = context.widgets.set("late", textView("late-widget"));
		context.widgets.clear("status");
		context.chrome.setHeader(textView("late-header"));
		context.chrome.setFooter(textView("late-footer"));
		context.chrome.setTitle("Late title");
		context.editor.setText("late");
		context.editor.insertText(" text");
		context.editor.setView(textView("late-editor"));
		context.dialogs.notify("Late notification");
		context.toolResults.setRenderer("late", { renderCall: () => textView("late-tool") });
		context.advanced.close();
		expect(context.editor.getText()).toBe("");
		expect(afterDisposeWidget.mounted).toBe(false);
		await expect(context.dialogs.select({ title: "Late select", options: [] })).resolves.toBeUndefined();
		await expect(context.dialogs.confirm({ title: "Late confirm", message: "No" })).resolves.toBe(false);
		await expect(context.dialogs.input({ title: "Late input" })).resolves.toBeUndefined();
		await expect(context.editor.open({ title: "Late editor input" })).resolves.toBeUndefined();
		await expect(context.advanced.show(() => textView("late advanced"))).resolves.toBeUndefined();

		expect(getEditorText()).toBe("seed");
		expect(notify).not.toHaveBeenCalled();
		expect(title).not.toHaveBeenCalled();
		expect(host.getToolRenderer("late")).toBeUndefined();
		expect(requestRender).not.toHaveBeenCalled();
	});

	test("cancels an asynchronous advanced view before it can mount after disposal", async () => {
		const { renderer, host } = await setup();
		let resolveView: ((view: ExtensionUIView) => void) | undefined;
		const advanced = host.context.advanced.show(
			() =>
				new Promise<ExtensionUIView>((resolve) => {
					resolveView = resolve;
				}),
		);
		host.dispose();
		resolveView?.(textView("resolved-too-late"));

		await expect(advanced).resolves.toBeUndefined();
		await setupByRenderer.get(renderer)?.flush();
		expect(setupByRenderer.get(renderer)?.captureCharFrame()).not.toContain("resolved-too-late");
	});

	test("closes a mounted advanced overlay when disposed", async () => {
		const { renderer, host } = await setup();
		const advanced = host.context.advanced.show(() => textView("mounted-advanced"));
		expect(await flushUntil(renderer, "mounted-advanced")).toContain("mounted-advanced");

		host.dispose();

		await expect(advanced).resolves.toBeUndefined();
		await setupByRenderer.get(renderer)?.flush();
		expect(setupByRenderer.get(renderer)?.captureCharFrame()).not.toContain("mounted-advanced");
	});
});
