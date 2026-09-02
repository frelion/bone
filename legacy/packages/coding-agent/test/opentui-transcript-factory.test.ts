import { readFileSync } from "node:fs";
import type { AssistantMessage, ImageContent } from "@frelion/bone-ai";
import { type BoxRenderable, type CliRenderer, type Renderable, TextRenderable } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSessionEvent } from "../src/core/agent-session.ts";
import type { ActionItem } from "../src/core/exchange/index.ts";
import type { ExtensionUIViewFactory } from "../src/core/extensions/ui-v2.ts";
import { decodeOpenTUIImage } from "../src/modes/interactive/components/opentui-image.ts";
import { OpenTUITranscriptFactory } from "../src/modes/interactive/components/opentui-transcript-factory.ts";
import { initTheme, theme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<TestRendererSetup>();
const projectedActions = new WeakMap<OpenTUITranscriptFactory, ActionItem[]>();
let nativeRenderer: CliRenderer;
let nativeSetup: TestRendererSetup;

function applyProjectedActions(
	factory: OpenTUITranscriptFactory,
	actions: ActionItem[],
): ReturnType<OpenTUITranscriptFactory["applyExchangeProjection"]> {
	projectedActions.set(factory, actions);
	return factory.applyExchangeProjection({
		sessionId: "test-session",
		activeExchangeId: "test-exchange",
		exchanges: [
			{
				id: "test-exchange",
				sessionId: "test-session",
				status: "running",
				inputs: [],
				modelTurns: [],
				items: actions,
				startedAt: 0,
			},
		],
	});
}

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

function setActionCall(id: string, title: string, timestamp: number): AssistantMessage {
	return {
		...assistant(""),
		content: [{ type: "toolCall", id, name: "set_action", arguments: { title } }],
		timestamp,
	};
}

function setActionResult(id: string, timestamp: number) {
	return {
		role: "toolResult" as const,
		toolName: "set_action",
		toolCallId: id,
		content: [{ type: "text" as const, text: "Action active." }],
		isError: false,
		timestamp,
	};
}

async function startLiveAction(factory: OpenTUITranscriptFactory, actionId: string, title: string) {
	const previous = projectedActions.get(factory) ?? [];
	const actions = [
		...previous.map((action) =>
			action.status === "in_progress"
				? { ...action, status: "completed" as const, completedAt: action.startedAt + 1 }
				: action,
		),
		{
			type: "action" as const,
			id: actionId,
			kind: "semantic",
			label: title,
			status: "in_progress" as const,
			modelTurnIds: [],
			toolCalls: [],
			sequence: previous.length,
			startedAt: previous.length + 1,
		},
	];
	const mutation = applyProjectedActions(factory, actions);
	await factory.handleEvent({
		type: "tool_execution_start",
		toolCallId: actionId,
		toolName: "set_action",
		args: { title },
	});
	await factory.handleEvent({
		type: "tool_execution_end",
		toolCallId: actionId,
		toolName: "set_action",
		result: { content: [{ type: "text", text: "Action active." }], details: {} },
		isError: false,
	});
	return mutation;
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

	test("hides protocol correction ToolResults in direct and persisted replay", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const protocolResult = {
			role: "toolResult" as const,
			toolCallId: "rejected-read",
			toolName: "read",
			content: [{ type: "text" as const, text: "Agent protocol error [ACTION_REQUIRED]" }],
			details: {
				internal: { kind: "agent_protocol_error" as const, code: "ACTION_REQUIRED", attempt: 1, maxAttempts: 3 },
			},
			isError: true,
			timestamp: 2,
		};
		expect(await factory.createMessage(protocolResult)).toBeUndefined();

		const items = await factory.createSessionEntries([
			{
				type: "message",
				id: "rejected-assistant",
				parentId: null,
				timestamp: "2026-07-22T00:00:00.000Z",
				message: {
					...assistant(""),
					content: [{ type: "toolCall", id: "rejected-read", name: "read", arguments: { path: "secret.ts" } }],
				},
			},
			{
				type: "message",
				id: "protocol-result",
				parentId: "rejected-assistant",
				timestamp: "2026-07-22T00:00:01.000Z",
				message: protocolResult,
			},
			{
				type: "message",
				id: "final",
				parentId: "protocol-result",
				timestamp: "2026-07-22T00:00:02.000Z",
				message: assistant("corrected"),
			},
		]);
		expect(items).toHaveLength(1);
		expect(items[0]?.key).toBe("final");
	});

	test("does not infer a live Action when an unguarded tool event violates the protocol invariant", async () => {
		const onError = vi.fn();
		const factory = new OpenTUITranscriptFactory(nativeRenderer, {}, { onError });
		const mutation = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "unguarded",
			toolName: "read",
			args: { path: "unexpected.ts" },
		});
		expect(mutation).toEqual({ type: "ignored" });
		expect(onError).toHaveBeenCalledWith(expect.any(Error), "live tool execution");
	});

	test("waits for the Exchange projection instead of inferring an Action from set_action events", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		expect(
			await factory.handleEvent({
				type: "tool_execution_start",
				toolCallId: "projected-action",
				toolName: "set_action",
				args: { title: "Inspecting projection state" },
			}),
		).toEqual({ type: "ignored" });

		const projected = await startLiveAction(factory, "projected-action", "Inspecting projection state");
		expect(projected.type).toBe("append");
	});

	test("projects child-agent state into its owning Action character frame", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const projected = applyProjectedActions(factory, [
			{
				type: "action",
				id: "delegated-action",
				kind: "semantic",
				label: "Reviewing runtime",
				status: "in_progress",
				toolCalls: [],
				startedAt: 1,
			},
		]);
		if (projected.type !== "append") throw new Error("Expected projected action append");
		nativeRenderer.root.add(projected.item.root);
		factory.applySubagentProjection({
			executions: [
				{
					agentRef: "child-1",
					label: "Concurrency review",
					scope: "exchange",
					status: "running",
					yields: [],
					unreadYieldCount: 0,
					origin: {
						exchangeId: "test-exchange",
						actionId: "delegated-action",
						toolCallId: "delegate-call",
					},
					createdAt: 1,
					lastActivityAt: 2,
				},
			],
		});
		await nativeSetup.flush();
		const frame = nativeSetup.captureCharFrame();
		expect(frame).toContain("Reviewing runtime");
		expect(frame).toContain("◐ running");
		expect(frame).toContain("Concurrency review");
	});

	test("uses projected ToolCall ownership while replaying buffered events across an Action switch", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const projected = applyProjectedActions(factory, [
			{
				type: "action",
				id: "earlier-action",
				kind: "semantic",
				label: "Inspecting earlier files",
				status: "completed",
				modelTurnIds: ["turn-1"],
				toolCalls: [
					{
						id: "buffered-tool",
						modelTurnId: "turn-1",
						toolName: "read",
						status: "completed",
						arguments: { path: "earlier.ts" },
						sequence: 0,
						startedAt: 1,
						completedAt: 2,
					},
				],
				sequence: 0,
				startedAt: 1,
				completedAt: 2,
			},
			{
				type: "action",
				id: "current-action",
				kind: "semantic",
				label: "Inspecting current files",
				status: "in_progress",
				modelTurnIds: [],
				toolCalls: [],
				sequence: 1,
				startedAt: 3,
			},
		]);
		if (projected.type !== "append") throw new Error("expected projected Action group");

		await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "buffered-tool",
			toolName: "read",
			args: { path: "earlier.ts" },
		});

		const details = (projected.item.root as BoxRenderable).getChildren()[1] as BoxRenderable;
		const earlierAction = details.getChildren()[0] as BoxRenderable;
		const currentAction = details.getChildren()[1] as BoxRenderable;
		const earlierTools = earlierAction.getChildren()[1] as BoxRenderable;
		const currentTools = currentAction.getChildren()[1] as BoxRenderable;
		expect(earlierTools.getChildren()).toHaveLength(1);
		expect(currentTools.getChildren()).toHaveLength(0);
	});

	test("groups consecutive persisted tool results during batch replay", async () => {
		initTheme("dark");
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const entries = await factory.createSessionEntries([
			{
				type: "message",
				id: "replay-action-call",
				parentId: null,
				timestamp: "2026-07-22T00:00:00.000Z",
				message: setActionCall("replay-action-control", "Reading files", 900),
			},
			{
				type: "message",
				id: "replay-action-result",
				parentId: "replay-action-call",
				timestamp: "2026-07-22T00:00:00.000Z",
				message: setActionResult("replay-action-control", 950),
			},
			{
				type: "message",
				id: "assistant-tool-1",
				parentId: "replay-action-result",
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
		expect(entries[0]?.key).toBe("working-group:replay:replay-action-call");

		const setup = setupAt(90, 18);
		const renderer = setup.renderer;
		if (!entries[0]) throw new Error("expected replay working group");
		renderer.root.add(entries[0].root);
		await frame(setup, "Reading files");
		const details = (entries[0].root as BoxRenderable).getChildren()[1] as BoxRenderable;
		const actionRoot = details.getChildren()[0] as BoxRenderable;
		const actionTitle = actionRoot.getChildren()[0];
		if (!actionTitle) throw new Error("expected replay Action title");
		await setup.mockMouse.click(actionTitle.screenX + 1, actionTitle.screenY);
		const captured = await frame(setup, "read · two.txt · complete");
		expect(captured).toContain("read · one.txt · complete");
		expect(captured).not.toContain("first result");
		expect(captured).not.toContain("second result");
	});

	test("resumes the persisted active Action before applying live ToolCall events", async () => {
		const onError = vi.fn();
		const factory = new OpenTUITranscriptFactory(nativeRenderer, {}, { onError });
		const entries = await factory.createSessionEntries(
			[
				{
					type: "message",
					id: "active-action-call",
					parentId: null,
					timestamp: "2026-07-22T00:00:00.000Z",
					message: setActionCall("active-action", "Reading active files", 1),
				},
				{
					type: "message",
					id: "active-action-result",
					parentId: "active-action-call",
					timestamp: "2026-07-22T00:00:00.100Z",
					message: setActionResult("active-action", 2),
				},
			],
			{ activeActionId: "active-action" },
		);

		expect(entries).toHaveLength(1);
		await startLiveAction(factory, "active-action", "Reading active files");
		const tool = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "active-read",
			toolName: "read",
			args: { path: "active.ts" },
		});
		expect(tool).toMatchObject({ type: "updated", key: entries[0]?.key });
		expect(onError).not.toHaveBeenCalled();
	});

	test("replays set_action as one semantic Action and hides its control ToolCall", async () => {
		initTheme("dark");
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const entries = await factory.createSessionEntries([
			{
				type: "message",
				id: "set-action-call",
				parentId: null,
				timestamp: "2026-07-22T00:00:00.000Z",
				message: {
					...assistant(""),
					content: [
						{
							type: "text",
							text: "I will inspect the request handlers first.",
							textSignature: JSON.stringify({ v: 1, id: "stage-1", phase: "commentary" }),
						},
						{ type: "toolCall", id: "control-1", name: "set_action", arguments: { title: "Inspecting files" } },
					],
					timestamp: 1,
				},
			},
			{
				type: "message",
				id: "set-action-result",
				parentId: "set-action-call",
				timestamp: "2026-07-22T00:00:00.100Z",
				message: {
					role: "toolResult",
					toolName: "set_action",
					toolCallId: "control-1",
					content: [{ type: "text", text: "ok" }],
					isError: false,
					timestamp: 2,
				},
			},
			{
				type: "message",
				id: "real-calls",
				parentId: "set-action-result",
				timestamp: "2026-07-22T00:00:01.000Z",
				message: {
					...assistant(""),
					content: [
						{ type: "toolCall", id: "read-a", name: "read", arguments: { path: "a.ts" } },
						{ type: "toolCall", id: "read-b", name: "read", arguments: { path: "b.ts" } },
					],
					timestamp: 3,
				},
			},
			...[
				["read-a", "a.ts"],
				["read-b", "b.ts"],
			].map(([id, path], index) => ({
				type: "message" as const,
				id: `result-${id}`,
				parentId: "real-calls",
				timestamp: `2026-07-22T00:00:0${index + 2}.000Z`,
				message: {
					role: "toolResult" as const,
					toolName: "read",
					toolCallId: id,
					content: [
						{
							type: "text" as const,
							text: id === "read-a" ? "a.ts failed\nA-ERROR-DETAIL" : `${path} result`,
						},
					],
					isError: id === "read-a",
					timestamp: index + 4,
				},
			})),
		]);

		expect(entries).toHaveLength(2);
		expect(entries[0]?.key).toBe("set-action-call");
		expect(entries[1]?.key).toBe("working-group:replay:set-action-call");
		const setup = setupAt(90, 20);
		if (!entries[0] || !entries[1]) throw new Error("expected stage update followed by replay Action group");
		setup.renderer.root.add(entries[0].root);
		setup.renderer.root.add(entries[1].root);
		let captured = await frame(setup, "Inspecting files");
		expect(captured.indexOf("I will inspect the request handlers first.")).toBeLessThan(
			captured.indexOf("Inspecting files"),
		);
		expect(captured).not.toMatch(/set_action|read · a\.ts|read · b\.ts|a\.ts result/);

		const details = (entries[1].root as BoxRenderable).getChildren()[1] as BoxRenderable;
		const actionRoot = details.getChildren()[0] as BoxRenderable;
		const actionTitle = actionRoot.getChildren()[0] as TextRenderable;
		if (!actionTitle) throw new Error("expected replay Action title");
		const expectedTitleColor = theme.getFgColor("toolTitle");
		const titleColor = actionTitle.fg as { buffer?: Uint16Array } | undefined;
		expect(Array.from(titleColor?.buffer ?? [])).toEqual([
			Number.parseInt(expectedTitleColor.slice(1, 3), 16),
			Number.parseInt(expectedTitleColor.slice(3, 5), 16),
			Number.parseInt(expectedTitleColor.slice(5, 7), 16),
			255,
		]);
		await setup.mockMouse.click(actionTitle.screenX + 1, actionTitle.screenY);
		captured = await frame(setup, "read · b.ts · complete");
		expect(captured).toContain("read · a.ts · failed: a.ts failed");
		expect(captured).not.toContain("A-ERROR-DETAIL");
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
				id: "action-a",
				parentId: null,
				timestamp: "2026-07-22T00:00:00.000Z",
				message: setActionCall("control-a", "Reading A", 0),
			},
			{
				type: "message",
				id: "action-result-a",
				parentId: "action-a",
				timestamp: "2026-07-22T00:00:00.100Z",
				message: setActionResult("control-a", 0),
			},
			{
				type: "message",
				id: "assistant-a",
				parentId: "action-result-a",
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
				id: "action-b",
				parentId: "assistant-text",
				timestamp: "2026-07-22T00:00:02.100Z",
				message: setActionCall("control-b", "Reading B", 3),
			},
			{
				type: "message",
				id: "action-result-b",
				parentId: "action-b",
				timestamp: "2026-07-22T00:00:02.200Z",
				message: setActionResult("control-b", 3),
			},
			{
				type: "message",
				id: "assistant-b",
				parentId: "action-result-b",
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
			"working-group:replay:action-a",
			"assistant-text",
			"working-group:replay:action-b",
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
				id: "action-before-limit",
				parentId: null,
				timestamp: "2026-07-22T00:00:00.000Z",
				message: setActionCall("control-before-limit", "Reading before limit", 0),
			},
			{
				type: "message",
				id: "action-result-before-limit",
				parentId: "action-before-limit",
				timestamp: "2026-07-22T00:00:00.100Z",
				message: setActionResult("control-before-limit", 0),
			},
			{
				type: "message",
				id: "assistant-before-limit",
				parentId: "action-result-before-limit",
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
				id: "action-after-limit",
				parentId: "result-before-limit",
				timestamp: "2026-07-22T00:00:01.100Z",
				message: setActionCall("control-after-limit", "Reading after limit", 2),
			},
			{
				type: "message",
				id: "action-result-after-limit",
				parentId: "action-after-limit",
				timestamp: "2026-07-22T00:00:01.200Z",
				message: setActionResult("control-after-limit", 2),
			},
			{
				type: "message",
				id: "assistant-limit",
				parentId: "action-result-after-limit",
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
			"working-group:replay:action-before-limit",
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
				id: "thinking-action",
				parentId: null,
				timestamp: "2026-07-22T00:00:00.000Z",
				message: setActionCall("thinking-control", "Inspecting dependency", 0),
			},
			{
				type: "message",
				id: "thinking-action-result",
				parentId: "thinking-action",
				timestamp: "2026-07-22T00:00:00.100Z",
				message: setActionResult("thinking-control", 0),
			},
			{
				type: "message",
				id: "assistant-tool",
				parentId: "thinking-action-result",
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
				id: "next-thinking-action",
				parentId: "result-tool",
				timestamp: "2026-07-22T00:00:01.100Z",
				message: setActionCall("next-thinking-control", "Inspecting next dependency", 2),
			},
			{
				type: "message",
				id: "next-thinking-action-result",
				parentId: "next-thinking-action",
				timestamp: "2026-07-22T00:00:01.200Z",
				message: setActionResult("next-thinking-control", 2),
			},
			{
				type: "message",
				id: "assistant-thinking",
				parentId: "next-thinking-action-result",
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

		expect(entries.map((entry) => entry.key)).toEqual(["working-group:replay:thinking-action", "assistant-thinking"]);
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
		const started = await startLiveAction(factory, "future-expanded-action", "Reading expanded.txt");
		if (started.type !== "append") throw new Error("expected Action group");
		await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "future-expanded-tool",
			toolName: "read",
			args: { path: "expanded.txt" },
		});
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
		expect(captured).toContain("read · expanded.txt · complete");
		expect(captured).not.toContain("Working");
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

		const actionStart = await startLiveAction(factory, "stable-tool-action", "Reading README.md");
		expect(actionStart.type).toBe("append");
		if (actionStart.type !== "append") throw new Error("expected Action append");
		renderer.root.add(actionStart.item.root);
		const toolStart = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "call-1",
			toolName: "read",
			args: { path: "README.md" },
		});
		expect(toolStart.type).toBe("updated");
		const toolEnd = await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "call-1",
			toolName: "read",
			result: { content: [{ type: "text", text: "done" }], details: {} },
			isError: false,
		});
		expect(toolEnd.type).toBe("updated");
		if (toolEnd.type !== "updated") throw new Error("expected tool update");
		expect(toolEnd.root).toBe(actionStart.item.root);
		const collapsed = await frame(setup, "read · README.md · complete");
		expect(collapsed).not.toContain("done");
		factory.setAllToolDetailsExpanded(true);
		expect(await frame(setup, "done")).toContain("read · README.md · complete");

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

	test("finalizes live markdown when partial messages already have a stop reason", async () => {
		const complete =
			"Mixed Markdown streaming should preserve normal wrapping across English and 中文 text, including **bold text**, a [link](https://example.com/long/path), and `inline code`. STREAM-LAYOUT-END";
		const chunks = complete.match(/.{1,4}/gu) ?? [];
		const setup = setupAt(60, 16);
		const factory = new OpenTUITranscriptFactory(setup.renderer);
		let streamed = chunks[0] ?? "";
		const started = await factory.handleEvent({ type: "message_start", message: assistant(streamed) });
		if (started.type !== "append") throw new Error("expected assistant append");
		setup.renderer.root.add(started.item.root);

		for (const chunk of chunks.slice(1)) {
			streamed += chunk;
			await factory.handleEvent({
				type: "message_update",
				message: assistant(streamed),
				assistantMessageEvent: {
					type: "text_delta",
					contentIndex: 0,
					delta: chunk,
					partial: assistant(streamed),
				},
			});
		}
		await factory.handleEvent({ type: "message_end", message: assistant(complete) });
		const finalFrame = await frame(setup, "STREAM-LAYOUT-END");

		const replaySetup = await createTestRenderer({ width: 60, height: 16 });
		renderers.add(replaySetup);
		const replayFactory = new OpenTUITranscriptFactory(replaySetup.renderer);
		const replayed = await replayFactory.createMessage(assistant(complete));
		if (!replayed) throw new Error("expected replayed assistant");
		replaySetup.renderer.root.add(replayed.root);
		const replayFrame = await frame(replaySetup, "STREAM-LAYOUT-END");

		expect(finalFrame).toBe(replayFrame);
	});

	test("uses structured tool renderers with stable transcript identity and state", async () => {
		initTheme("dark");
		const setup = setupAt(90, 26);
		const renderer = setup.renderer;
		const states: unknown[] = [];
		const previousViews: Array<Renderable | undefined> = [];
		const detailAnchors: Renderable[] = [];
		const renderCall = vi.fn((args: unknown, context: { expanded: boolean }) =>
			textView(`custom call:${context.expanded}:${JSON.stringify(args)}`),
		);
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
				onToolDetailChange: (anchor, mutate) => {
					detailAnchors.push(anchor);
					mutate();
				},
			},
		);
		const started = await startLiveAction(factory, "structured-tool-action", "Reading one.txt");
		if (started.type !== "append") throw new Error("expected Action append");
		await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "call-custom",
			toolName: "read",
			args: { path: "one.txt" },
		});
		renderer.root.add(started.item.root);
		expect(await frame(setup, "read · one.txt · running")).not.toContain("custom call");
		const groupDetails = (started.item.root as BoxRenderable).getChildren()[1] as BoxRenderable;
		const actionRoot = groupDetails.getChildren()[0] as BoxRenderable;
		const actionTitle = actionRoot.getChildren()[0];
		const toolsRoot = actionRoot.getChildren()[1] as BoxRenderable;
		if (!actionTitle) throw new Error("expected action title");
		await setup.mockMouse.click(actionTitle.screenX + 1, actionTitle.screenY);
		await setup.flush();
		const structuredRoot = toolsRoot.getChildren()[0] as BoxRenderable;
		const fallbackRoot = structuredRoot.getChildren()[0] as BoxRenderable;
		const fallbackBody = fallbackRoot.getChildren()[0] as BoxRenderable;
		const toolTitle = fallbackBody.getChildren()[0];
		if (!toolTitle) throw new Error("expected structured tool title");
		await setup.mockMouse.click(toolTitle.screenX + 1, toolTitle.screenY);
		expect(await frame(setup, "custom call:true")).toContain("one.txt");
		expect(detailAnchors).toEqual([actionTitle, toolTitle]);
		expect(fallbackBody.getChildren()[0] as Renderable | undefined).toBe(toolTitle);
		expect(renderCall).toHaveBeenLastCalledWith(expect.anything(), expect.objectContaining({ expanded: true }));
		await setup.mockMouse.click(toolTitle.screenX + 1, toolTitle.screenY);
		expect(await frame(setup, "read · one.txt · running")).not.toContain("custom call");
		expect(detailAnchors).toEqual([actionTitle, toolTitle, toolTitle]);
		expect(fallbackBody.getChildren()[0] as Renderable | undefined).toBe(toolTitle);

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

	test("uses renderer-provided summaries in the collapsed tool transcript", async () => {
		initTheme("dark");
		const setup = setupAt(90, 20);
		const summarize = vi.fn(({ phase }: { phase: string }) => `Forge issue · created #42 · ${phase}`);
		const factory = new OpenTUITranscriptFactory(nativeRenderer, {}, { getToolRenderer: () => ({ summarize }) });
		const started = await startLiveAction(factory, "forge-summary-action", "Creating Forge issue");
		if (started.type !== "append") throw new Error("expected Action append");
		await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "forge-summary",
			toolName: "forge_issue",
			args: { action: "create" },
		});
		setup.renderer.root.add(started.item.root);
		const details = (started.item.root as BoxRenderable).getChildren()[1] as BoxRenderable;
		const actionRoot = details.getChildren()[0] as BoxRenderable;
		const actionTitle = actionRoot.getChildren()[0];
		if (!actionTitle) throw new Error("expected Action title");
		await frame(setup, "Creating Forge issue");
		await setup.mockMouse.click(actionTitle.screenX + 1, actionTitle.screenY);
		await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "forge-summary",
			toolName: "forge_issue",
			result: { content: [{ type: "text", text: "{}" }], details: { number: 42 } },
			isError: false,
		});
		const captured = await frame(setup, "Forge issue · created #42 · complete");
		expect(captured).toContain("Forge issue · created #42 · complete");
		expect(summarize).toHaveBeenLastCalledWith(
			expect.objectContaining({ phase: "complete", args: { action: "create" }, isError: false }),
		);
	});

	test("groups consecutive live tool calls into one stable semantic Action", async () => {
		let now = 0;
		const factory = new OpenTUITranscriptFactory(nativeRenderer, { now: () => now });
		const started = await startLiveAction(factory, "group-action", "Reading related files");
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
		expect(started.type).toBe("append");
		expect(first.type).toBe("updated");
		expect(second.type).toBe("updated");
		if (started.type !== "append" || first.type !== "updated" || second.type !== "updated") {
			throw new Error("expected one Action group");
		}
		expect(first.key).toBe(started.item.key);
		expect(second.key).toBe(started.item.key);
		expect(second.root).toBe(started.item.root);

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
		expect(completed.root).toBe(started.item.root);

		initTheme("dark");
		const setup = setupAt(90, 18);
		const renderer = setup.renderer;
		renderer.root.add(started.item.root);
		const details = (started.item.root as BoxRenderable).getChildren()[1] as BoxRenderable;
		const actionRoot = details.getChildren()[0] as BoxRenderable;
		const actionTitle = actionRoot.getChildren()[0];
		if (!actionTitle) throw new Error("expected Action title");
		await frame(setup, "Reading related files");
		await setup.mockMouse.click(actionTitle.screenX + 1, actionTitle.screenY);
		const captured = await frame(setup, "read · two.txt · complete");
		expect(captured).toContain("read · one.txt · complete");
		expect(captured).not.toContain("Working");
	});

	test("places live Agent activity after the user and updates it through commentary and tools", async () => {
		let now = 0;
		const factory = new OpenTUITranscriptFactory(nativeRenderer, { now: () => now });
		const setup = setupAt(100, 24);
		const renderer = setup.renderer;

		expect((await factory.handleEvent({ type: "agent_start" })).type).toBe("ignored");
		const user = { role: "user" as const, content: "Check the event queue", timestamp: 1 };
		const userStart = await factory.handleEvent({ type: "message_start", message: user });
		expect(userStart.type).toBe("append");
		if (userStart.type !== "append") throw new Error("expected user append");
		renderer.root.add(userStart.item.root);

		const activity = await factory.handleEvent({ type: "message_end", message: user });
		expect(activity.type).toBe("append");
		if (activity.type !== "append") throw new Error("expected initial activity");
		renderer.root.add(activity.item.root);
		let captured = await frame(setup, "Working");
		expect(captured).not.toMatch(/◐|◓|◑|◒/);

		const commentary = {
			...assistant("Inspecting the stream event queue"),
			content: [
				{
					type: "text" as const,
					text: "Inspecting the stream event queue",
					textSignature: JSON.stringify({ v: 1, id: "commentary-1", phase: "commentary" }),
				},
			],
		};
		const emptyCommentary = { ...commentary, content: [] };
		const assistantStart = await factory.handleEvent({ type: "message_start", message: emptyCommentary });
		expect(assistantStart.type).toBe("append");
		if (assistantStart.type === "append") renderer.root.add(assistantStart.item.root);
		const commentaryUpdate = await factory.handleEvent({
			type: "message_update",
			message: commentary,
			assistantMessageEvent: {
				type: "text_delta",
				contentIndex: 0,
				delta: "Inspecting the stream event queue",
				partial: commentary,
			},
		});
		if (commentaryUpdate.type === "append") renderer.root.add(commentaryUpdate.item.root);
		captured = await frame(setup, "Inspecting the stream event queue");
		expect(captured).toContain("Inspecting the stream event queue");
		expect(captured).toContain("Working");

		const actionStart = await startLiveAction(factory, "activity-action", "Inspecting events.ts");
		expect(actionStart.type).toBe("updated");
		const toolStart = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "activity-call",
			toolName: "read",
			args: { path: "events.ts" },
		});
		expect(toolStart.type).toBe("updated");
		captured = await frame(setup, "Inspecting events.ts");
		expect(captured.indexOf("Inspecting the stream event queue")).toBeLessThan(
			captured.indexOf("Inspecting events.ts"),
		);
		expect(captured).not.toContain("Working");

		await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "activity-call",
			toolName: "read",
			result: { content: [{ type: "text", text: "event queue result" }], details: {} },
			isError: false,
		});
		factory.setAllToolDetailsExpanded(true);
		captured = await frame(setup, "event queue result");
		expect(captured).toContain("read · events.ts · complete");
		expect(captured).toContain("events.ts");
		expect(captured).not.toContain("Working");
		now = 4_000;
		const ended = await factory.handleEvent({ type: "agent_end", messages: [commentary], willRetry: false });
		expect(ended.type).toBe("updated");
		captured = await frame(setup, "Inspecting the stream event queue");
		expect(captured).toContain("read · events.ts · complete");
		expect(captured).not.toContain("Working");
	});

	test("groups multiple live ToolCalls under one semantic Action", async () => {
		let now = 0;
		const factory = new OpenTUITranscriptFactory(nativeRenderer, { now: () => now });
		const setup = setupAt(90, 22);
		const started = await startLiveAction(factory, "semantic-1", "Inspecting request handlers");
		if (started.type !== "append") throw new Error("expected semantic Action append");
		setup.renderer.root.add(started.item.root);
		await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "semantic-read-a",
			toolName: "read",
			args: { path: "a.ts" },
		});
		await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "semantic-read-b",
			toolName: "read",
			args: { path: "b.ts" },
		});
		let captured = await frame(setup, "Inspecting request handlers");
		expect(captured).not.toMatch(/read · a\.ts|read · b\.ts|Working/);

		const details = (started.item.root as BoxRenderable).getChildren()[1] as BoxRenderable;
		const actionRoot = details.getChildren()[0] as BoxRenderable;
		const actionTitle = actionRoot.getChildren()[0];
		if (!actionTitle) throw new Error("expected semantic Action title");
		await setup.mockMouse.click(actionTitle.screenX + 1, actionTitle.screenY);
		captured = await frame(setup, "read · b.ts · running");
		expect(captured).toContain("read · a.ts · running");

		for (const [toolCallId, path] of [
			["semantic-read-a", "a.ts"],
			["semantic-read-b", "b.ts"],
		] as const) {
			await factory.handleEvent({
				type: "tool_execution_end",
				toolCallId,
				toolName: "read",
				result: { content: [{ type: "text", text: `${path} result` }], details: {} },
				isError: false,
			});
		}
		expect(await frame(setup, "read · b.ts · complete")).not.toContain("Working");
		now = 2_000;
		await factory.handleEvent({ type: "agent_end", messages: [], willRetry: false });
		captured = await frame(setup, "Inspecting request handlers");
		expect(captured).not.toContain("Working");
	});

	test("completes the Action group after a final answer starts a new activity group", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const started = await startLiveAction(factory, "final-action", "Reading final input");
		if (started.type !== "append") throw new Error("expected Action group");
		await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "final-tool",
			toolName: "read",
			args: { path: "final.txt" },
		});
		await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "final-tool",
			toolName: "read",
			result: { content: [{ type: "text", text: "done" }], details: {} },
			isError: false,
		});
		const finalMessage = assistant("Final answer");
		await factory.handleEvent({ type: "message_start", message: finalMessage });
		const completed = applyProjectedActions(
			factory,
			(projectedActions.get(factory) ?? []).map((action) => ({
				...action,
				status: "completed",
				completedAt: action.startedAt + 1,
			})),
		);
		await factory.handleEvent({ type: "message_end", message: finalMessage });
		expect(completed).toMatchObject({ type: "updated", key: started.item.key });

		const ended = await factory.handleEvent({ type: "agent_end", messages: [finalMessage], willRetry: false });
		expect(ended.type).toBe("updated");
	});

	test("keeps an already expanded extension action open when it fails", async () => {
		const setup = setupAt(90, 20);
		const renderer = setup.renderer;
		const factory = new OpenTUITranscriptFactory(
			nativeRenderer,
			{},
			{
				getToolRenderer: () => ({
					renderCall: (_args, context) => textView(`custom call:${context.expanded}`),
					renderResult: (input, context) =>
						textView(
							`custom result:${context.expanded}:${input.result.content.map((part) => (part.type === "text" ? part.text : "")).join("")}`,
						),
				}),
			},
		);
		const started = await startLiveAction(factory, "expanded-failure-action", "Creating Forge issue");
		if (started.type !== "append") throw new Error("expected Action append");
		await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "expanded-failure",
			toolName: "forge_issue",
			args: { action: "create" },
		});
		renderer.root.add(started.item.root);
		await frame(setup, "forge_issue · create · running");
		const groupDetails = (started.item.root as BoxRenderable).getChildren()[1] as BoxRenderable;
		const actionRoot = groupDetails.getChildren()[0] as BoxRenderable;
		const actionTitle = actionRoot.getChildren()[0];
		const toolsRoot = actionRoot.getChildren()[1] as BoxRenderable;
		if (!actionTitle) throw new Error("expected action summary");
		await setup.mockMouse.click(actionTitle.screenX + 1, actionTitle.screenY);
		await frame(setup, "forge_issue · create · running");
		const structuredRoot = toolsRoot.getChildren()[0] as BoxRenderable;
		const fallbackRoot = structuredRoot.getChildren()[0] as BoxRenderable;
		const fallbackBody = fallbackRoot.getChildren()[0] as BoxRenderable;
		const title = fallbackBody.getChildren()[0];
		if (!title) throw new Error("expected ToolCall summary");
		await setup.mockMouse.click(title.screenX + 1, title.screenY);
		expect(await frame(setup, "custom call:true")).toContain("custom call:true");

		await factory.handleEvent({
			type: "tool_execution_end",
			toolCallId: "expanded-failure",
			toolName: "forge_issue",
			result: { content: [{ type: "text", text: "API rejected request" }], details: {} },
			isError: true,
		});
		expect(await frame(setup, "custom result:true:API rejected request")).toContain(
			"custom result:true:API rejected request",
		);
	});

	test("keeps each phase update ahead of the actions performed for that phase", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const setup = setupAt(100, 24);
		const renderer = setup.renderer;
		const commentary = (id: string, text: string): AssistantMessage => ({
			...assistant(""),
			content: [
				{
					type: "text",
					text,
					textSignature: JSON.stringify({ v: 1, id, phase: "commentary" }),
				},
			],
		});
		const runAction = async (id: string, path: string): Promise<void> => {
			await startLiveAction(factory, `${id}-action`, `Read ${path}`);
			await factory.handleEvent({
				type: "tool_execution_start",
				toolCallId: id,
				toolName: "read",
				args: { path },
			});
			await factory.handleEvent({
				type: "tool_execution_end",
				toolCallId: id,
				toolName: "read",
				result: { content: [{ type: "text", text: `${path} result` }], details: {} },
				isError: false,
			});
		};

		await factory.handleEvent({ type: "agent_start" });
		const firstPhase = commentary("phase-1", "Inspecting the request path");
		const firstPhaseStart = await factory.handleEvent({ type: "message_start", message: firstPhase });
		if (firstPhaseStart.type !== "append") throw new Error("expected first phase");
		renderer.root.add(firstPhaseStart.item.root);
		await factory.handleEvent({ type: "message_end", message: firstPhase });
		await runAction("phase-1-action-1", "router.ts");
		await runAction("phase-1-action-2", "handler.ts");

		const secondPhase = commentary("phase-2", "Verifying the updated behavior");
		const secondPhaseStart = await factory.handleEvent({ type: "message_start", message: secondPhase });
		if (secondPhaseStart.type !== "append") throw new Error("expected second phase");
		renderer.root.add(secondPhaseStart.item.root);
		await factory.handleEvent({ type: "message_end", message: secondPhase });
		await runAction("phase-2-action-1", "router.test.ts");

		const captured = await frame(setup, "Read router.test.ts");
		const firstPhaseIndex = captured.indexOf("Inspecting the request path");
		const firstActionIndex = captured.indexOf("Read router.ts");
		const secondActionIndex = captured.indexOf("Read handler.ts");
		const secondPhaseIndex = captured.indexOf("Verifying the updated behavior");
		const finalActionIndex = captured.indexOf("Read router.test.ts");
		expect(firstPhaseIndex).toBeGreaterThanOrEqual(0);
		expect(firstPhaseIndex).toBeLessThan(firstActionIndex);
		expect(firstActionIndex).toBeLessThan(secondActionIndex);
		expect(secondActionIndex).toBeLessThan(secondPhaseIndex);
		expect(secondPhaseIndex).toBeLessThan(finalActionIndex);
		expect(captured).not.toContain("Working");
		expect(captured).not.toMatch(/◐|◓|◑|◒/);
	});

	test("replaces the initial Working state with the first explicit semantic Action", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const setup = setupAt(90, 14);
		const renderer = setup.renderer;

		await factory.handleEvent({ type: "agent_start" });
		const user = { role: "user" as const, content: "Inspect the router", timestamp: 1 };
		const userStart = await factory.handleEvent({ type: "message_start", message: user });
		if (userStart.type !== "append") throw new Error("expected user message");
		renderer.root.add(userStart.item.root);
		const activity = await factory.handleEvent({ type: "message_end", message: user });
		if (activity.type !== "append") throw new Error("expected initial activity");
		renderer.root.add(activity.item.root);
		expect(await frame(setup, "Working")).not.toMatch(/◐|◓|◑|◒/);

		const action = await startLiveAction(factory, "first-action", "Inspecting the router");
		expect(action.type).toBe("updated");
		const tool = await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "first-action-tool",
			toolName: "read",
			args: { path: "router.ts" },
		});
		expect(tool.type).toBe("updated");
		const captured = await frame(setup, "Inspecting the router");
		expect(captured).not.toContain("Working");
	});

	test("shows a fresh static activity for a follow-up in the same Agent run", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const setup = setupAt(100, 24);
		const renderer = setup.renderer;
		const appendEvent = async (event: AgentSessionEvent) => {
			const mutation = await factory.handleEvent(event);
			if (mutation.type === "append") renderer.root.add(mutation.item.root);
			return mutation;
		};

		await appendEvent({ type: "agent_start" });
		const initial = { role: "user" as const, content: "Initial request", timestamp: 1 };
		await appendEvent({ type: "message_start", message: initial });
		await appendEvent({ type: "message_end", message: initial });
		const response = assistant("First response");
		await appendEvent({ type: "message_start", message: response });
		await appendEvent({ type: "message_end", message: response });

		const followUp = { role: "user" as const, content: "Queued follow-up", timestamp: 2 };
		await appendEvent({ type: "message_start", message: followUp });
		const nextActivity = await appendEvent({ type: "message_end", message: followUp });
		expect(nextActivity.type).toBe("append");

		const captured = await frame(setup, "Working");
		expect(captured.indexOf("Initial request")).toBeLessThan(captured.indexOf("First response"));
		expect(captured.indexOf("First response")).toBeLessThan(captured.indexOf("Queued follow-up"));
		expect(captured.indexOf("Queued follow-up")).toBeLessThan(captured.indexOf("Working"));
		expect(captured).not.toMatch(/Completed work|◐|◓|◑|◒/);
	});

	test("keeps Agent activity alive across retry and reports a tool-free provider failure", async () => {
		let now = 0;
		const factory = new OpenTUITranscriptFactory(nativeRenderer, { now: () => now });
		const setup = setupAt(90, 16);
		const renderer = setup.renderer;

		await factory.handleEvent({ type: "agent_start" });
		const user = { role: "user" as const, content: "Retry this", timestamp: 1 };
		await factory.handleEvent({ type: "message_start", message: user });
		const activity = await factory.handleEvent({ type: "message_end", message: user });
		if (activity.type !== "append") throw new Error("expected initial activity");
		renderer.root.add(activity.item.root);

		const retrying = await factory.handleEvent({ type: "agent_end", messages: [], willRetry: true });
		expect(retrying.type).toBe("updated");
		expect(await frame(setup, "Retrying")).not.toContain("✓");
		await factory.handleEvent({
			type: "auto_retry_start",
			attempt: 1,
			maxAttempts: 3,
			delayMs: 100,
			errorMessage: "temporary failure",
		});
		expect(await frame(setup, "Retrying · 1/3")).not.toContain("✓");

		await factory.handleEvent({ type: "agent_start" });
		now = 3_000;
		const failedMessage = { ...assistant(""), stopReason: "error" as const, errorMessage: "provider failed" };
		await factory.handleEvent({ type: "agent_end", messages: [failedMessage], willRetry: false });
		const captured = await frame(setup, "Work failed");
		expect(captured).toContain("✗ Work failed · 3s");
		expect(captured).not.toContain("✓");
	});

	test("ends a live working group when text arrives after an empty assistant start", async () => {
		const factory = new OpenTUITranscriptFactory(nativeRenderer);
		const startAndComplete = async (id: string) => {
			const started = await startLiveAction(factory, `${id}-action`, `Read for ${id}`);
			await factory.handleEvent({
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
		factory.setAllToolDetailsExpanded(true);

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
		factory.setAllToolDetailsExpanded(true);

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

		const started = await startLiveAction(factory, "live-renderer-fallback-action", "Reading fallback.txt");
		if (started.type !== "append") throw new Error("expected Action append");
		await factory.handleEvent({
			type: "tool_execution_start",
			toolCallId: "live-tool",
			toolName: "read",
			args: { path: "fallback.txt" },
		});
		renderer.root.add(started.item.root);
		expect(await frame(setup, "read · fallback.txt · running")).not.toContain("complete fallback");

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
		expect(renderCall).not.toHaveBeenCalled();
		expect(renderResult).not.toHaveBeenCalled();
		expect(await frame(setup, "read · fallback.txt · complete")).not.toContain("complete fallback");
		factory.setAllToolDetailsExpanded(true);
		const toolFrame = await frame(setup, "complete fallback");
		expect(toolFrame).toContain("read · fallback.txt · complete");
		expect(toolFrame).not.toContain("partial fallback");
		expect(renderResult).toHaveBeenCalledOnce();

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
