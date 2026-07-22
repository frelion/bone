import {
	type BoneContainerNode,
	type BoneTestRenderer,
	type BoneView,
	createBoneTestRenderer,
} from "@frelion/bone-tui";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { ReadonlyFooterDataProvider } from "../src/core/footer-data-provider.ts";
import { OpenTUIExtensionHost } from "../src/modes/interactive/opentui-extension-host.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<BoneTestRenderer>();

function textView(content: string): BoneView {
	return { mount: (context) => context.createText({ content }) };
}

async function flushUntil(renderer: BoneTestRenderer, expected: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await renderer.flush();
		const frame = renderer.captureFrame();
		if (frame.includes(expected)) return frame;
	}
	return renderer.captureFrame();
}

async function setup() {
	const renderer = await createBoneTestRenderer({ width: 80, height: 28 });
	renderers.add(renderer);
	const root = renderer.createBox({ width: "100%", height: "100%", flexDirection: "column" });
	const region = (): BoneContainerNode => renderer.createBox({ width: "100%", flexDirection: "column" });
	const regions = {
		header: region(),
		aboveEditor: region(),
		editor: region(),
		belowEditor: region(),
		footer: region(),
	};
	for (const child of Object.values(regions)) root.append(child);
	renderer.content.append(root);
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
	return {
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

afterEach(() => {
	for (const renderer of renderers) renderer.destroy();
	renderers.clear();
});

describe("OpenTUIExtensionHost", () => {
	test("runs select, confirm, and input dialogs with the fixed keymap", async () => {
		const { renderer, host } = await setup();
		const selected = host.context.dialogs.select({
			title: "Choose provider",
			options: [
				{ value: "a", label: "Alpha", disabled: true },
				{ value: "b", label: "Beta" },
			],
		});
		expect(await flushUntil(renderer, "Choose provider")).toContain("Beta");
		renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		await renderer.flush();
		await expect(selected).resolves.toBe("b");

		const confirmed = host.context.dialogs.confirm({ title: "Remove", message: "Remove this item?" });
		expect(await flushUntil(renderer, "Remove this item?")).toContain("Confirm");
		renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		await expect(confirmed).resolves.toBe(false);

		const input = host.context.dialogs.input({ title: "Name", initialValue: "bo" });
		await flushUntil(renderer, "Name");
		await renderer.input.typeText("ne");
		renderer.input.pressEnter();
		await expect(input).resolves.toBe("bone");
	});

	test("cancels dialogs on escape, abort, and timeout", async () => {
		const { renderer, host } = await setup();
		const escaped = host.context.dialogs.input({ title: "Escape me" });
		await flushUntil(renderer, "Escape me");
		renderer.input.pressEscape();
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
		await renderer.flush();
		expect(renderer.captureFrame()).not.toContain("widget-updated");

		const header = host.context.chrome.setHeader(textView("custom-header"));
		host.context.chrome.setFooter((data) => textView(`branch:${data.getGitBranch()}`));
		host.context.editor.setView(textView("custom-editor"));
		frame = await flushUntil(renderer, "custom-editor");
		expect(frame).toContain("custom-header");
		expect(frame).toContain("branch:main");
		expect(header.mounted).toBe(true);
		refreshBranch();
		await renderer.flush();

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
		await renderer.flush();
		const frame = renderer.captureFrame();
		expect(frame).not.toContain("Pending input");
		expect(frame).not.toContain("active-widget");
		expect(frame).not.toContain("active-header");
		expect(frame).not.toContain("active-editor");
		expect(frame).not.toContain("must-not-remount");

		const mount = vi.spyOn(renderer, "mount");
		const showOverlay = vi.spyOn(renderer, "showOverlay");
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
		expect(mount).not.toHaveBeenCalled();
		expect(showOverlay).not.toHaveBeenCalled();
		expect(requestRender).not.toHaveBeenCalled();
	});

	test("cancels an asynchronous advanced view before it can mount after disposal", async () => {
		const { renderer, host } = await setup();
		let resolveView: ((view: BoneView) => void) | undefined;
		const advanced = host.context.advanced.show(
			() =>
				new Promise<BoneView>((resolve) => {
					resolveView = resolve;
				}),
		);
		host.dispose();
		resolveView?.(textView("resolved-too-late"));

		await expect(advanced).resolves.toBeUndefined();
		await renderer.flush();
		expect(renderer.captureFrame()).not.toContain("resolved-too-late");
	});

	test("closes a mounted advanced overlay when disposed", async () => {
		const { renderer, host } = await setup();
		const advanced = host.context.advanced.show(() => textView("mounted-advanced"));
		expect(await flushUntil(renderer, "mounted-advanced")).toContain("mounted-advanced");

		host.dispose();

		await expect(advanced).resolves.toBeUndefined();
		await renderer.flush();
		expect(renderer.captureFrame()).not.toContain("mounted-advanced");
	});
});
