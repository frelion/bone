import { readFileSync } from "node:fs";
import type { AssistantMessage, ImageContent } from "@frelion/bone-ai";
import { type CliRenderer, type Renderable, TextRenderable } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { ExtensionUIViewFactory } from "../src/core/extensions/ui-v2.ts";
import { decodeOpenTUIImage } from "../src/modes/interactive/components/opentui-image.ts";
import { OpenTUITranscriptFactory } from "../src/modes/interactive/components/opentui-transcript-factory.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<TestRendererSetup>();
let nativeRenderer: CliRenderer;
let nativeSetup: TestRendererSetup;

function textView(content: string): ExtensionUIViewFactory {
	return (renderer) => new TextRenderable(renderer, { content });
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

async function frame(setup: TestRendererSetup, expected: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await setup.flush();
		const captured = setup.captureCharFrame();
		if (captured.includes(expected)) return captured;
	}
	return setup.captureCharFrame();
}

beforeEach(async () => {
	initTheme("dark");
	const setup = await createTestRenderer({ width: 100, height: 32 });
	renderers.add(setup);
	nativeSetup = setup;
	nativeRenderer = setup.renderer;
});

function setupAt(width: number, height: number): TestRendererSetup {
	nativeSetup.resize(width, height);
	return nativeSetup;
}

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUI transcript factory", () => {
	test("maps persisted entries and intentionally ignores metadata entries", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
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

	test("groups consecutive persisted tool results during batch replay", async () => {
		initTheme("dark");
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const entries = await factory.createSessionEntries([
			{
				type: "message",
				id: "assistant-tool-1",
				parentId: null,
				timestamp: "2026-07-22T00:00:00.000Z",
				message: {
					...assistant(""),
					content: [{ type: "toolCall", id: "replay-1", name: "read", arguments: { path: "one.txt" } }],
					timestamp: 1_000,
				},
			},
			{
				type: "message",
				id: "tool-entry-1",
				parentId: "assistant-tool-1",
				timestamp: "2026-07-22T00:00:00.000Z",
				message: {
					role: "toolResult",
					toolName: "read",
					toolCallId: "replay-1",
					content: [{ type: "text", text: "first result" }],
					isError: false,
					timestamp: 1_000,
				},
			},
			{
				type: "message",
				id: "assistant-tool-2",
				parentId: "tool-entry-1",
				timestamp: "2026-07-22T00:00:09.000Z",
				message: {
					...assistant(""),
					content: [{ type: "toolCall", id: "replay-2", name: "read", arguments: { path: "two.txt" } }],
					timestamp: 10_000,
				},
			},
			{
				type: "message",
				id: "tool-entry-2",
				parentId: "tool-entry-1",
				timestamp: "2026-07-22T00:00:18.000Z",
				message: {
					role: "toolResult",
					toolName: "read",
					toolCallId: "replay-2",
					content: [{ type: "text", text: "second result" }],
					isError: false,
					timestamp: 19_000,
				},
			},
		]);
		expect(entries).toHaveLength(1);
		expect(entries[0]?.key).toBe("working-group:replay:assistant-tool-1");

		const setup = setupAt(90, 18);
		const renderer = setup.renderer;
		if (!entries[0]) throw new Error("expected replay working group");
		renderer.root.add(entries[0].root);
		const captured = await frame(setup, "✓ Worked for 18s · 2 tool calls");
		expect(captured).not.toContain("first result");
		expect(captured).not.toContain("second result");
	});

	test("does not merge replay working groups across visible assistant text", async () => {
		initTheme("dark");
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const toolAssistant = (id: string, timestamp: number): AssistantMessage => ({
			...assistant(""),
			content: [{ type: "toolCall", id, name: "read", arguments: {} }],
			timestamp,
		});
		const toolResult = (id: string, timestamp: number) => ({
			role: "toolResult" as const,
			toolName: "read",
			toolCallId: id,
			content: [{ type: "text" as const, text: id }],
			isError: false,
			timestamp,
		});
		const entries = await factory.createSessionEntries([
			{
				type: "message",
				id: "assistant-a",
				parentId: null,
				timestamp: "2026-07-22T00:00:00.000Z",
				message: toolAssistant("call-a", 1),
			},
			{
				type: "message",
				id: "result-a",
				parentId: "assistant-a",
				timestamp: "2026-07-22T00:00:01.000Z",
				message: toolResult("call-a", 2),
			},
			{
				type: "message",
				id: "assistant-text",
				parentId: "result-a",
				timestamp: "2026-07-22T00:00:02.000Z",
				message: { ...assistant("A visible boundary"), timestamp: 3 },
			},
			{
				type: "message",
				id: "assistant-b",
				parentId: "assistant-text",
				timestamp: "2026-07-22T00:00:03.000Z",
				message: toolAssistant("call-b", 4),
			},
			{
				type: "message",
				id: "result-b",
				parentId: "assistant-b",
				timestamp: "2026-07-22T00:00:04.000Z",
				message: toolResult("call-b", 5),
			},
		]);

		expect(entries.map((entry) => entry.key)).toEqual([
			"working-group:replay:assistant-a",
			"assistant-text",
			"working-group:replay:assistant-b",
		]);
	});

	test("preserves a visible length error on a tool-call assistant during replay", async () => {
		initTheme("dark");
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const toolAssistant = (
			id: string,
			timestamp: number,
			stopReason?: AssistantMessage["stopReason"],
		): AssistantMessage => ({
			...assistant(""),
			content: [{ type: "toolCall", id, name: "read", arguments: {} }],
			stopReason,
			timestamp,
		});
		const toolResult = (id: string, timestamp: number) => ({
			role: "toolResult" as const,
			toolName: "read",
			toolCallId: id,
			content: [{ type: "text" as const, text: `${id} result` }],
			isError: false,
			timestamp,
		});
		const entries = await factory.createSessionEntries([
			{
				type: "message",
				id: "assistant-before-limit",
				parentId: null,
				timestamp: "2026-07-22T00:00:00.000Z",
				message: toolAssistant("call-before-limit", 1),
			},
			{
				type: "message",
				id: "result-before-limit",
				parentId: "assistant-before-limit",
				timestamp: "2026-07-22T00:00:01.000Z",
				message: toolResult("call-before-limit", 2),
			},
			{
				type: "message",
				id: "assistant-limit",
				parentId: "result-before-limit",
				timestamp: "2026-07-22T00:00:02.000Z",
				message: toolAssistant("call-after-limit", 3, "length"),
			},
			{
				type: "message",
				id: "result-after-limit",
				parentId: "assistant-limit",
				timestamp: "2026-07-22T00:00:03.000Z",
				message: toolResult("call-after-limit", 4),
			},
		]);

		expect(entries.map((entry) => entry.key)).toEqual([
			"working-group:replay:assistant-before-limit",
			"assistant-limit",
			"working-group:replay:result-after-limit",
		]);
		const setup = setupAt(100, 18);
		const renderer = setup.renderer;
		const limitEntry = entries[1];
		if (!limitEntry) throw new Error("expected visible length error");
		renderer.root.add(limitEntry.root);
		expect(await frame(setup, "maximum output token limit")).toContain("Error: Model stopped");
	});

	test("preserves streaming thinking on a tool-call assistant during replay", async () => {
		initTheme("dark");
		const factory = new OpenTUITranscriptFactory(nativeRenderer, {
			hideThinkingBlock: true,
			hiddenThinkingLabel: "Reasoning...",
		});
		const entries = await factory.createSessionEntries([
			{
				type: "message",
				id: "assistant-tool",
				parentId: null,
				timestamp: "2026-07-22T00:00:00.000Z",
				message: {
					...assistant(""),
					content: [{ type: "toolCall", id: "call-thinking", name: "read", arguments: {} }],
					timestamp: 1,
				},
			},
			{
				type: "message",
				id: "result-tool",
				parentId: "assistant-tool",
				timestamp: "2026-07-22T00:00:01.000Z",
				message: {
					role: "toolResult",
					toolName: "read",
					toolCallId: "call-thinking",
					content: [{ type: "text", text: "done" }],
					isError: false,
					timestamp: 2,
				},
			},
			{
				type: "message",
				id: "assistant-thinking",
				parentId: "result-tool",
				timestamp: "2026-07-22T00:00:02.000Z",
				message: {
					...assistant(""),
					content: [
						{ type: "thinking", thinking: "Inspecting the next dependency" },
						{ type: "toolCall", id: "call-next", name: "read", arguments: {} },
					],
					stopReason: undefined,
					timestamp: 3,
				},
			},
		]);

		expect(entries.map((entry) => entry.key)).toEqual(["working-group:replay:assistant-tool", "assistant-thinking"]);
		const setup = setupAt(90, 14);
		const renderer = setup.renderer;
		const thinkingEntry = entries[1];
		if (!thinkingEntry) throw new Error("expected visible thinking entry");
		renderer.root.add(thinkingEntry.root);
		expect(await frame(setup, "Reasoning...")).toContain("Reasoning...");
	});

	test("keeps future successful groups expanded while the global override is active", async () => {
		initTheme("dark");
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		factory.setAllToolDetailsExpanded(true);
		const started = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "future-expanded-tool",
			toolName: "read",
			args: { path: "expanded.txt" },
		});
		if (started.type !== "append") throw new Error("expected working group");
		await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "future-expanded-tool",
			toolName: "read",
			result: { content: [{ type: "text", text: "future detail" }], details: {} },
			isError: false,
		});

		const setup = setupAt(90, 18);
		const renderer = setup.renderer;
		renderer.root.add(started.item.root);
		const captured = await frame(setup, "future detail");
		expect(captured).toContain("read · complete");
		expect(captured).toContain("⌄ ✓ Inspected the workspace · 1s · 1 tool call");
	});

	test("keeps stable assistant and tool views through streaming updates", async () => {
		initTheme("dark");
		const setup = setupAt(90, 26);
		const renderer = setup.renderer;
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const started = await factory.handleEvent({ type: "message_start", message: assistant("first") });
		expect(started.type).toBe("append");
		if (started.type !== "append") throw new Error("expected append");
		renderer.root.add(started.item.root);
		const updated = await factory.handleEvent({
			type: "message_update",
			message: assistant("second"),
			assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "second", partial: assistant("second") },
		});
		expect(updated.type).toBe("updated");
		if (updated.type !== "updated") throw new Error("expected update");
		expect(updated.root).toBe(started.item.root);
		expect(await frame(setup, "second")).not.toContain("first");

		const toolStart = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "call-1",
			toolName: "read",
			args: { path: "README.md" },
		});
		expect(toolStart.type).toBe("append");
		if (toolStart.type !== "append") throw new Error("expected tool append");
		renderer.root.add(toolStart.item.root);
		const toolEnd = await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "call-1",
			toolName: "read",
			result: { content: [{ type: "text", text: "done" }], details: {} },
			isError: false,
		});
		expect(toolEnd.type).toBe("updated");
		if (toolEnd.type !== "updated") throw new Error("expected tool update");
		expect(toolEnd.root).toBe(toolStart.item.root);
		const collapsed = await frame(setup, "Worked for 1s · 1 tool calls");
		expect(collapsed).not.toContain("done");
		factory.setAllToolDetailsExpanded(true);
		expect(await frame(setup, "done")).toContain("read · complete");

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
		const setup = setupAt(90, 26);
		const renderer = setup.renderer;
		const states: unknown[] = [];
		const previousViews: Array<Renderable | undefined> = [];
		const renderCall = vi.fn((args: unknown) => textView(`custom call:${JSON.stringify(args)}`));
		const renderResult = vi.fn(
			(
				input: { result: { content: Array<{ type: string; text?: string }> } },
				context: { state: unknown; previousView?: Renderable },
			) => {
				states.push(context.state);
				previousViews.push(context.previousView);
				const output = input.result.content.map((part) => part.text ?? "").join("");
				return textView(`custom result:${output}`);
			},
		);
		const factory = new OpenTUITranscriptFactory(
			nativeRenderer,
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
		renderer.root.add(started.item.root);
		expect(await frame(setup, "custom call")).toContain("one.txt");

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
		expect(partial.root).toBe(started.item.root);
		expect(completed.root).toBe(started.item.root);
		expect(states[1]).toBe(states[0]);
		expect(previousViews.every(Boolean)).toBe(true);
		expect(await frame(setup, "custom result:complete")).not.toContain("custom result:partial");
	});

	test("groups consecutive live tool calls into one stable working group", async () => {
		let now = 0;
		const factory = new OpenTUITranscriptFactory(nativeRenderer, { now: () => now });
		const first = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "group-call-1",
			toolName: "read",
			args: { path: "one.txt" },
		});
		const second = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "group-call-2",
			toolName: "read",
			args: { path: "two.txt" },
		});
		expect(first.type).toBe("append");
		expect(second.type).toBe("updated");
		if (first.type !== "append" || second.type !== "updated") throw new Error("expected one working group");
		expect(second.key).toBe(first.item.key);
		expect(second.root).toBe(first.item.root);

		await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "group-call-1",
			toolName: "read",
			result: { content: [{ type: "text", text: "one" }], details: {} },
			isError: false,
		});
		now = 18_000;
		const completed = await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "group-call-2",
			toolName: "read",
			result: { content: [{ type: "text", text: "two" }], details: {} },
			isError: false,
		});
		expect(completed.type).toBe("updated");
		if (completed.type !== "updated") throw new Error("expected group update");
		expect(completed.root).toBe(first.item.root);

		initTheme("dark");
		const setup = setupAt(90, 18);
		const renderer = setup.renderer;
		renderer.root.add(first.item.root);
		const captured = await frame(setup, "✓ Worked for 18s · 2 tool calls");
		expect(captured).not.toContain("one.txt");
		expect(captured).not.toContain("two.txt");
	});

	test("ends a live working group when text arrives after an empty assistant start", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const startAndComplete = async (id: string) => {
			const started = await factory.handleEvent({
				type: "tool_execution_start",
				toolCallId: id,
				toolName: "read",
				args: {},
			});
			await factory.handleEvent({
				type: "tool_execution_end",
				toolCallId: id,
				toolName: "read",
				result: { content: [{ type: "text", text: id }], details: {} },
				isError: false,
			});
			return started;
		};

		const first = await startAndComplete("before-update");
		await factory.handleEvent({ type: "message_start", message: { ...assistant(""), content: [] } });
		await factory.handleEvent({
			type: "message_update",
			message: assistant("visible update"),
			assistantMessageEvent: {
				type: "text_delta",
				contentIndex: 0,
				delta: "visible update",
				partial: assistant("visible update"),
			},
		});
		const second = await startAndComplete("after-update");
		if (first.type !== "append" || second.type !== "append") throw new Error("expected distinct groups");
		expect(second.item.key).not.toBe(first.item.key);

		await factory.handleEvent({ type: "message_start", message: { ...assistant(""), content: [] } });
		await factory.handleEvent({ type: "message_end", message: assistant("visible end") });
		const third = await startAndComplete("after-end");
		expect(third.type).toBe("append");
		if (third.type !== "append") throw new Error("expected third group");
		expect(third.item.key).not.toBe(second.item.key);
	});

	test("uses registered custom message and session entry views with fallback behavior", async () => {
		const messageView = vi.fn(() => textView("registered message"));
		const entryView = vi.fn(() => textView("registered entry"));
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
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
		expect(customMessage?.root).toBeDefined();
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
		const setup = setupAt(80, 16);
		const renderer = setup.renderer;
		if (!fallback) throw new Error("expected custom message fallback");
		renderer.root.add(fallback.root);
		expect(await frame(setup, "fallback content")).toContain("fallback content");
	});

	test("isolates throwing custom and tool renderers while replaying history", async () => {
		initTheme("dark");
		const setup = setupAt(90, 20);
		const renderer = setup.renderer;
		const factory = new OpenTUITranscriptFactory(
			nativeRenderer,
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
		renderer.root.add(custom.root);
		renderer.root.add(tool.root);

		const captured = await frame(setup, "generic tool fallback");
		expect(captured).toContain("generic custom fallback");
		expect(captured).toContain("read · complete");
		expect(captured).toContain("generic tool fallback");
	});

	test("falls back and reports extension views that throw while mounting", async () => {
		initTheme("dark");
		const setup = setupAt(90, 22);
		const renderer = setup.renderer;
		const toolError = new Error("tool view mount failed");
		const messageError = new Error("custom message view mount failed");
		const entryError = new Error("custom entry view mount failed");
		const onError = vi.fn();
		const throwingView =
			(error: Error): ExtensionUIViewFactory =>
			() => {
				throw error;
			};
		const factory = new OpenTUITranscriptFactory(
			nativeRenderer,
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
		renderer.root.add(tool.root);
		renderer.root.add(customMessage.root);
		renderer.root.add(customEntry.root);

		const captured = await frame(setup, "[custom entry unavailable]");
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
		const setup = setupAt(90, 22);
		const renderer = setup.renderer;
		const renderCall = vi.fn(() => {
			throw new Error("tool call renderer failed");
		});
		const renderResult = vi.fn(() => {
			throw new Error("tool result renderer failed");
		});
		const factory = new OpenTUITranscriptFactory(
			nativeRenderer,
			{},
			{ getToolRenderer: () => ({ renderCall, renderResult }) },
		);

		const started = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "live-tool",
			toolName: "read",
			args: { path: "fallback.txt" },
		});
		expect(started.type).toBe("append");
		if (started.type !== "append") throw new Error("expected tool append");
		renderer.root.add(started.item.root);
		expect(await frame(setup, "fallback.txt")).toContain("read · running");

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
		expect(await frame(setup, "Worked for 1s · 1 tool calls")).not.toContain("complete fallback");
		factory.setAllToolDetailsExpanded(true);
		const toolFrame = await frame(setup, "complete fallback");
		expect(toolFrame).toContain("read · complete");
		expect(toolFrame).not.toContain("partial fallback");

		const following = await factory.handleEvent({ type: "message_start", message: assistant("still alive") });
		expect(following.type).toBe("append");
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
