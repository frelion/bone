import { readFileSync } from "node:fs";
import type { AssistantMessage, ImageContent } from "@frelion/bone-ai";
import { type BoneView, createBoneTestRenderer } from "@frelion/bone-tui";
import { afterEach, describe, expect, test, vi } from "vitest";
import { decodeOpenTUIImage } from "../src/modes/interactive/components/opentui-image.ts";
import { OpenTUITranscriptFactory } from "../src/modes/interactive/components/opentui-transcript-factory.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<Awaited<ReturnType<typeof createBoneTestRenderer>>>();

function textView(content: string): BoneView {
	return { mount: (context) => context.createText({ content }) };
}

function assistant(text: string): AssistantMessage {
	return {
		role: "assistant",
		content: [{ type: "text", text }],
		api: "openai-responses",
		provider: "openai",
		model: "test",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "stop",
		timestamp: 10,
	};
}

async function frame(renderer: Awaited<ReturnType<typeof createBoneTestRenderer>>, expected: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await renderer.flush();
		const captured = renderer.captureFrame();
		if (captured.includes(expected)) return captured;
	}
	return renderer.captureFrame();
}

afterEach(() => {
	for (const renderer of renderers) renderer.destroy();
	renderers.clear();
});

describe("OpenTUI transcript factory", () => {
	test("maps persisted entries and intentionally ignores metadata entries", async () => {
		const factory = new OpenTUITranscriptFactory();
		const message = await factory.createSessionEntry({
			type: "message",
			id: "entry-1",
			parentId: null,
			timestamp: "2026-07-22T00:00:00.000Z",
			message: { role: "user", content: "hello", timestamp: 1 },
		});
		expect(message?.key).toBe("entry-1");
		const metadata = await factory.createSessionEntry({
			type: "model_change",
			id: "entry-2",
			parentId: "entry-1",
			timestamp: "2026-07-22T00:00:01.000Z",
			provider: "openai",
			modelId: "test",
		});
		expect(metadata).toBeUndefined();
		const hidden = await factory.createSessionEntry({
			type: "custom_message",
			id: "entry-3",
			parentId: "entry-2",
			timestamp: "2026-07-22T00:00:02.000Z",
			customType: "private",
			content: "hidden",
			display: false,
		});
		expect(hidden).toBeUndefined();
	});

	test("keeps stable assistant and tool views through streaming updates", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 90, height: 26 });
		renderers.add(renderer);
		renderer.start();
		const factory = new OpenTUITranscriptFactory();
		const started = await factory.handleEvent({ type: "message_start", message: assistant("first") });
		expect(started.type).toBe("append");
		if (started.type !== "append") throw new Error("expected append");
		renderer.content.append(started.item.view.mount(renderer));
		const updated = await factory.handleEvent({
			type: "message_update",
			message: assistant("second"),
			assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "second", partial: assistant("second") },
		});
		expect(updated.type).toBe("updated");
		if (updated.type !== "updated") throw new Error("expected update");
		expect(updated.view).toBe(started.item.view);
		expect(await frame(renderer, "second")).not.toContain("first");

		const toolStart = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "call-1",
			toolName: "read",
			args: { path: "README.md" },
		});
		expect(toolStart.type).toBe("append");
		if (toolStart.type !== "append") throw new Error("expected tool append");
		renderer.content.append(toolStart.item.view.mount(renderer));
		const toolEnd = await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "call-1",
			toolName: "read",
			result: { content: [{ type: "text", text: "done" }], details: {} },
			isError: false,
		});
		expect(toolEnd.type).toBe("updated");
		if (toolEnd.type !== "updated") throw new Error("expected tool update");
		expect(toolEnd.view).toBe(toolStart.item.view);
		expect(await frame(renderer, "done")).toContain("read · complete");

		const duplicateResult = await factory.handleEvent({
			type: "message_start",
			message: {
				role: "toolResult",
				toolName: "read",
				toolCallId: "call-1",
				content: [{ type: "text", text: "done" }],
				details: {},
				isError: false,
				timestamp: 11,
			},
		});
		expect(duplicateResult).toEqual({ type: "ignored" });

		const replayedResult = await factory.createMessage({
			role: "toolResult",
			toolName: "read",
			toolCallId: "call-1",
			content: [{ type: "text", text: "done" }],
			details: {},
			isError: false,
			timestamp: 11,
		});
		expect(replayedResult?.key).toBe("tool:call-1");
	});

	test("uses structured tool renderers with stable transcript identity and state", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 90, height: 26 });
		renderers.add(renderer);
		renderer.start();
		const states: unknown[] = [];
		const previousViews: Array<BoneView | undefined> = [];
		const renderCall = vi.fn((args: unknown) => textView(`custom call:${JSON.stringify(args)}`));
		const renderResult = vi.fn(
			(
				input: { result: { content: Array<{ type: string; text?: string }> } },
				context: { state: unknown; previousView?: BoneView },
			) => {
				states.push(context.state);
				previousViews.push(context.previousView);
				const output = input.result.content.map((part) => part.text ?? "").join("");
				return textView(`custom result:${output}`);
			},
		);
		const factory = new OpenTUITranscriptFactory(
			{},
			{
				cwd: "/workspace",
				getToolRenderer: (toolName) => (toolName === "read" ? { renderCall, renderResult } : undefined),
			},
		);
		const started = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "call-custom",
			toolName: "read",
			args: { path: "one.txt" },
		});
		expect(started.type).toBe("append");
		if (started.type !== "append") throw new Error("expected tool append");
		renderer.content.append(started.item.view.mount(renderer));
		expect(await frame(renderer, "custom call")).toContain("one.txt");

		const partial = await factory.handleEvent({
			type: "tool_execution_update",
			toolCallId: "call-custom",
			toolName: "read",
			args: { path: "two.txt" },
			partialResult: { content: [{ type: "text", text: "partial" }], details: { count: 1 } },
		});
		const completed = await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "call-custom",
			toolName: "read",
			result: { content: [{ type: "text", text: "complete" }], details: { count: 2 } },
			isError: false,
		});
		expect(partial.type).toBe("updated");
		expect(completed.type).toBe("updated");
		if (partial.type !== "updated" || completed.type !== "updated") throw new Error("expected tool updates");
		expect(partial.view).toBe(started.item.view);
		expect(completed.view).toBe(started.item.view);
		expect(states[1]).toBe(states[0]);
		expect(previousViews.every(Boolean)).toBe(true);
		expect(await frame(renderer, "custom result:complete")).not.toContain("custom result:partial");
	});

	test("uses registered custom message and session entry views with fallback behavior", async () => {
		const messageView = vi.fn(() => textView("registered message"));
		const entryView = vi.fn(() => textView("registered entry"));
		const factory = new OpenTUITranscriptFactory();
		factory.setResolvers({
			getMessageView: (customType) => (customType === "notice" ? messageView : undefined),
			getEntryView: (customType) => (customType === "state" ? entryView : undefined),
		});

		const customMessage = await factory.createMessage({
			role: "custom",
			customType: "notice",
			content: "default content",
			display: true,
			timestamp: 1,
		});
		expect(customMessage?.view).toBeDefined();
		expect(messageView).toHaveBeenCalledWith(expect.objectContaining({ customType: "notice" }), { expanded: false });

		const customEntry = await factory.createSessionEntry({
			type: "custom",
			id: "entry-custom",
			parentId: null,
			timestamp: "2026-07-22T00:00:00.000Z",
			customType: "state",
			data: { ready: true },
		});
		expect(customEntry?.key).toBe("entry-custom");
		expect(entryView).toHaveBeenCalledWith(expect.objectContaining({ customType: "state" }), { expanded: false });
		const liveEntry = await factory.handleEvent({
			type: "entry_appended",
			entry: {
				type: "custom",
				id: "entry-live",
				parentId: "entry-custom",
				timestamp: "2026-07-22T00:00:01.000Z",
				customType: "state",
				data: { ready: false },
			},
		});
		expect(liveEntry.type).toBe("append");

		const fallback = await factory.createMessage({
			role: "custom",
			customType: "unregistered",
			content: "fallback content",
			display: true,
			timestamp: 2,
		});
		const renderer = await createBoneTestRenderer({ width: 80, height: 16 });
		renderers.add(renderer);
		renderer.start();
		if (!fallback) throw new Error("expected custom message fallback");
		renderer.content.append(fallback.view.mount(renderer));
		expect(await frame(renderer, "fallback content")).toContain("fallback content");
	});

	test("isolates throwing custom and tool renderers while replaying history", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 90, height: 20 });
		renderers.add(renderer);
		renderer.start();
		const factory = new OpenTUITranscriptFactory(
			{},
			{
				getMessageView: () => () => {
					throw new Error("custom message renderer failed");
				},
				getToolRenderer: () => ({
					renderResult: () => {
						throw new Error("tool result renderer failed");
					},
				}),
			},
		);

		const custom = await factory.createMessage({
			role: "custom",
			customType: "notice",
			content: "generic custom fallback",
			display: true,
			timestamp: 1,
		});
		const tool = await factory.createMessage({
			role: "toolResult",
			toolName: "read",
			toolCallId: "replayed-tool",
			content: [{ type: "text", text: "generic tool fallback" }],
			details: {},
			isError: false,
			timestamp: 2,
		});
		if (!custom || !tool) throw new Error("expected replay fallbacks");
		renderer.content.append(custom.view.mount(renderer));
		renderer.content.append(tool.view.mount(renderer));

		const captured = await frame(renderer, "generic tool fallback");
		expect(captured).toContain("generic custom fallback");
		expect(captured).toContain("read · complete");
		expect(captured).toContain("generic tool fallback");
	});

	test("falls back and reports extension views that throw while mounting", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 90, height: 22 });
		renderers.add(renderer);
		renderer.start();
		const toolError = new Error("tool view mount failed");
		const messageError = new Error("custom message view mount failed");
		const entryError = new Error("custom entry view mount failed");
		const onError = vi.fn();
		const throwingView = (error: Error): BoneView => ({
			mount: () => {
				throw error;
			},
		});
		const factory = new OpenTUITranscriptFactory(
			{},
			{
				getToolRenderer: () => ({ renderResult: () => throwingView(toolError) }),
				getMessageView: () => () => throwingView(messageError),
				getEntryView: () => () => throwingView(entryError),
				onError,
			},
		);

		const tool = await factory.createMessage({
			role: "toolResult",
			toolName: "read",
			toolCallId: "mount-failure-tool",
			content: [{ type: "text", text: "tool mount fallback" }],
			details: {},
			isError: false,
			timestamp: 3,
		});
		const customMessage = await factory.createMessage({
			role: "custom",
			customType: "notice",
			content: "message mount fallback",
			display: true,
			timestamp: 4,
		});
		const customEntry = await factory.createSessionEntry({
			type: "custom",
			id: "mount-failure-entry",
			parentId: null,
			timestamp: "2026-07-22T00:00:00.000Z",
			customType: "state",
			data: { ready: true },
		});
		if (!tool || !customMessage || !customEntry) throw new Error("expected extension views");
		renderer.content.append(tool.view.mount(renderer));
		renderer.content.append(customMessage.view.mount(renderer));
		renderer.content.append(customEntry.view.mount(renderer));

		const captured = await frame(renderer, "[custom entry unavailable]");
		expect(captured).toContain("read · complete");
		expect(captured).toContain("tool mount fallback");
		expect(captured).toContain("message mount fallback");
		expect(captured).toContain("[custom entry unavailable]");
		expect(onError).toHaveBeenCalledTimes(3);
		expect(onError).toHaveBeenCalledWith(toolError, "tool renderer view");
		expect(onError).toHaveBeenCalledWith(messageError, "custom message view");
		expect(onError).toHaveBeenCalledWith(entryError, "custom entry view");
	});

	test("keeps processing live events after a structured tool renderer throws", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 90, height: 22 });
		renderers.add(renderer);
		renderer.start();
		const renderCall = vi.fn(() => {
			throw new Error("tool call renderer failed");
		});
		const renderResult = vi.fn(() => {
			throw new Error("tool result renderer failed");
		});
		const factory = new OpenTUITranscriptFactory({}, { getToolRenderer: () => ({ renderCall, renderResult }) });

		const started = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "live-tool",
			toolName: "read",
			args: { path: "fallback.txt" },
		});
		expect(started.type).toBe("append");
		if (started.type !== "append") throw new Error("expected tool append");
		renderer.content.append(started.item.view.mount(renderer));
		expect(await frame(renderer, "fallback.txt")).toContain("read · running");

		const partial = await factory.handleEvent({
			type: "tool_execution_update",
			toolCallId: "live-tool",
			toolName: "read",
			args: { path: "fallback.txt" },
			partialResult: { content: [{ type: "text", text: "partial fallback" }], details: {} },
		});
		const completed = await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "live-tool",
			toolName: "read",
			result: { content: [{ type: "text", text: "complete fallback" }], details: {} },
			isError: false,
		});
		expect(partial.type).toBe("updated");
		expect(completed.type).toBe("updated");
		expect(renderCall).toHaveBeenCalled();
		expect(renderResult).toHaveBeenCalledTimes(2);
		const toolFrame = await frame(renderer, "complete fallback");
		expect(toolFrame).toContain("read · complete");
		expect(toolFrame).not.toContain("partial fallback");

		const following = await factory.handleEvent({ type: "message_start", message: assistant("still alive") });
		expect(following.type).toBe("append");
		if (following.type !== "append") throw new Error("expected following assistant message");
		renderer.content.append(following.item.view.mount(renderer));
		expect(await frame(renderer, "still alive")).toContain("still alive");
	});

	test("decodes images to RGBA and returns an explicit fallback for corrupt input", async () => {
		const image: ImageContent = {
			type: "image",
			mimeType: "image/png",
			data: readFileSync(new URL("../src/modes/interactive/assets/clankolas.png", import.meta.url)).toString(
				"base64",
			),
		};
		const decoded = await decodeOpenTUIImage(image, { terminalWidth: 12 });
		expect(decoded.error).toBeUndefined();
		expect(decoded.pixelWidth).toBe(640);
		expect(decoded.pixelHeight).toBe(537);
		expect(decoded.pixels).toHaveLength(640 * 537 * 4);
		expect(decoded.terminalWidth).toBe(12);

		const corrupt = await decodeOpenTUIImage({ ...image, data: "not-an-image" });
		expect(corrupt.error).toBe("unsupported or corrupt image data");
	});
});
