import type { AgentTool } from "@frelion/bone-agent-core";
import { encodeTextSignature, fauxAssistantMessage, fauxToolCall } from "@frelion/bone-ai";
import { Type } from "typebox";
import { afterEach, describe, expect, it } from "vitest";
import type { SessionMessageEntry } from "../../src/core/session-manager.ts";
import { createHarness, getMessageText, type Harness, stageResponse } from "./harness.ts";

function messageEntries(harness: Harness): SessionMessageEntry[] {
	return harness.sessionManager.getEntries().filter((entry): entry is SessionMessageEntry => entry.type === "message");
}

function createWaitTool(): { tool: AgentTool; release: () => void } {
	let release: (() => void) | undefined;
	const waiting = new Promise<void>((resolve) => {
		release = resolve;
	});
	return {
		tool: {
			name: "wait",
			label: "Wait",
			description: "Wait for test release",
			parameters: Type.Object({}),
			execute: async () => {
				await waiting;
				return { content: [{ type: "text", text: "released" }], details: {} };
			},
		},
		release: () => release?.(),
	};
}

async function waitForToolStart(harness: Harness): Promise<void> {
	await new Promise<void>((resolve) => {
		const unsubscribe = harness.session.subscribe((event) => {
			if (event.type !== "tool_execution_start" || event.toolName !== "wait") return;
			unsubscribe();
			resolve();
		});
	});
}

describe("AgentSession exchange persistence metadata", () => {
	const harnesses: Harness[] = [];

	afterEach(() => {
		while (harnesses.length > 0) harnesses.pop()?.cleanup();
	});

	it("keeps steering in the current exchange while advancing the model turn", async () => {
		const { tool, release } = createWaitTool();
		const harness = await createHarness({ tools: [tool] });
		harnesses.push(harness);
		harness.setResponses([
			stageResponse("I will wait while steering is applied.", "Waiting for steering input"),
			fauxAssistantMessage(fauxToolCall("wait", {}), { stopReason: "toolUse" }),
			fauxAssistantMessage("handled steer"),
		]);

		const toolStarted = waitForToolStart(harness);
		const prompt = harness.session.prompt("initial task");
		await toolStarted;
		await harness.session.steer("adjust the task");
		release();
		await prompt;

		const entries = messageEntries(harness);
		const initial = entries.find((entry) => getMessageText(entry.message) === "initial task");
		const steer = entries.find((entry) => getMessageText(entry.message) === "adjust the task");
		const final = entries.find((entry) => getMessageText(entry.message) === "handled steer");
		expect(initial?.delivery).toBe("prompt");
		expect(steer?.delivery).toBe("steer");
		expect(steer?.exchangeId).toBe(initial?.exchangeId);
		expect(final?.exchangeId).toBe(initial?.exchangeId);
		expect(steer?.modelTurnId).not.toBe(initial?.modelTurnId);
		expect(final?.modelTurnId).toBe(steer?.modelTurnId);
		expect(final?.responseDisposition).toBe("final");
		expect(
			entries.find((entry) => getMessageText(entry.message).includes("wait while steering"))?.responseDisposition,
		).toBe("continuation");
		expect(harness.session.exchangeProjection.exchanges).toHaveLength(1);
		expect(harness.session.exchangeProjection.exchanges[0]?.inputs.map((input) => input.delivery)).toEqual([
			"prompt",
			"steer",
		]);
		expect(harness.session.exchangeProjection.exchanges[0]?.status).toBe("completed");
	});

	it("keeps one semantic Action across multiple ModelTurns and ToolCalls", async () => {
		const actionCallId = "action-stage-1-call";
		const probe: AgentTool = {
			name: "probe",
			label: "Probe",
			description: "Record one probe step",
			parameters: Type.Object({ step: Type.Number() }),
			execute: async (_toolCallId, { step }) => ({
				content: [{ type: "text", text: `step ${step}` }],
				details: { step },
			}),
		};
		const harness = await createHarness({ tools: [probe] });
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage(
				[
					{
						type: "text",
						text: "I will inspect the request path before changing it.",
						textSignature: encodeTextSignature("action-stage-1", "commentary"),
					},
					fauxToolCall("set_action", { title: "Inspecting request handlers" }, { id: actionCallId }),
				],
				{ stopReason: "toolUse" },
			),
			fauxAssistantMessage(fauxToolCall("probe", { step: 1 }), { stopReason: "toolUse" }),
			fauxAssistantMessage(fauxToolCall("probe", { step: 2 }), { stopReason: "toolUse" }),
			fauxAssistantMessage("inspection complete"),
		]);

		await harness.session.prompt("inspect the request path");

		const exchange = harness.session.exchangeProjection.exchanges[0];
		const actions = exchange?.items.filter((item) => item.type === "action") ?? [];
		expect(exchange?.items[0]).toMatchObject({
			type: "narrative",
			phase: "commentary",
			content: "I will inspect the request path before changing it.",
		});
		expect(actions).toHaveLength(1);
		expect(actions[0]).toMatchObject({
			id: actionCallId,
			label: "Inspecting request handlers",
			status: "completed",
			toolCalls: [
				{ toolName: "probe", status: "completed" },
				{ toolName: "probe", status: "completed" },
			],
		});
		if (actions[0]?.type !== "action") throw new Error("Expected a semantic Action");
		expect(actions[0].modelTurnIds).toHaveLength(2);
		expect(new Set(actions[0].modelTurnIds).size).toBe(2);
		expect(exchange?.items.at(-1)).toMatchObject({
			type: "narrative",
			phase: "final_answer",
			content: "inspection complete",
		});
	});

	it("allows several semantic Actions under one stage update", async () => {
		const probe: AgentTool = {
			name: "probe",
			label: "Probe",
			description: "Record one semantic step",
			parameters: Type.Object({ step: Type.String() }),
			execute: async (_toolCallId, { step }) => ({
				content: [{ type: "text", text: step }],
				details: { step },
			}),
		};
		const harness = await createHarness({ tools: [probe] });
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage(
				[
					{
						type: "text",
						text: "I will inspect the current design, then implement the change.",
						textSignature: encodeTextSignature("multi-action-stage-1", "commentary"),
					},
					fauxToolCall("set_action", { title: "Inspecting the current design" }),
				],
				{ stopReason: "toolUse" },
			),
			fauxAssistantMessage(fauxToolCall("probe", { step: "inspect" }), { stopReason: "toolUse" }),
			fauxAssistantMessage(fauxToolCall("set_action", { title: "Implementing the change" }), {
				stopReason: "toolUse",
			}),
			fauxAssistantMessage(fauxToolCall("probe", { step: "implement" }), { stopReason: "toolUse" }),
			fauxAssistantMessage("done"),
		]);

		await harness.session.prompt("inspect and implement");

		const items = harness.session.exchangeProjection.exchanges[0]?.items ?? [];
		const stageUpdates = items.filter((item) => item.type === "narrative" && item.phase === "commentary");
		const actions = items.filter((item) => item.type === "action");
		expect(stageUpdates).toMatchObject([
			{ content: "I will inspect the current design, then implement the change.", status: "completed" },
		]);
		expect(actions).toMatchObject([
			{
				label: "Inspecting the current design",
				status: "completed",
				toolCalls: [{ toolName: "probe", status: "completed", result: expect.anything() }],
			},
			{
				label: "Implementing the change",
				status: "completed",
				toolCalls: [{ toolName: "probe", status: "completed", result: expect.anything() }],
			},
		]);
		expect(items.at(-1)).toMatchObject({ type: "narrative", phase: "final_answer", content: "done" });
	});

	it("starts a new exchange when a queued follow-up is consumed", async () => {
		const { tool, release } = createWaitTool();
		const harness = await createHarness({ tools: [tool] });
		harnesses.push(harness);
		harness.session.setFollowUpMode("all");
		harness.setResponses([
			stageResponse("I will wait while follow-up work is queued.", "Waiting for follow-up work"),
			fauxAssistantMessage(fauxToolCall("wait", {}), { stopReason: "toolUse" }),
			fauxAssistantMessage("initial result"),
			fauxAssistantMessage("follow-up result"),
		]);

		const toolStarted = waitForToolStart(harness);
		const prompt = harness.session.prompt("initial task");
		await toolStarted;
		await harness.session.followUp("next task");
		await harness.session.followUp("another task");
		release();
		await prompt;

		const entries = messageEntries(harness);
		const initial = entries.find((entry) => getMessageText(entry.message) === "initial task");
		const initialResult = entries.find((entry) => getMessageText(entry.message) === "initial result");
		const followUp = entries.find((entry) => getMessageText(entry.message) === "next task");
		const batchedFollowUp = entries.find((entry) => getMessageText(entry.message) === "another task");
		const followUpResult = entries.find((entry) => getMessageText(entry.message) === "follow-up result");
		expect(followUp?.delivery).toBe("follow_up");
		expect(batchedFollowUp?.delivery).toBe("follow_up");
		expect(followUp?.exchangeId).not.toBe(initial?.exchangeId);
		expect(batchedFollowUp?.exchangeId).toBe(followUp?.exchangeId);
		expect(batchedFollowUp?.modelTurnId).toBe(followUp?.modelTurnId);
		expect(initialResult?.exchangeId).toBe(initial?.exchangeId);
		expect(followUpResult?.exchangeId).toBe(followUp?.exchangeId);
		expect(followUpResult?.modelTurnId).toBe(followUp?.modelTurnId);
		expect(harness.session.exchangeProjection.exchanges.map((exchange) => exchange.status)).toEqual([
			"completed",
			"completed",
		]);
		expect(harness.session.exchangeProjection.exchanges[1]?.inputs.map((input) => input.delivery)).toEqual([
			"follow_up",
			"follow_up",
		]);
	});

	it("keeps the exchange open when an incomplete response carried an explicit final phase", async () => {
		const harness = await createHarness({
			settings: { retry: { enabled: true, maxRetries: 1, baseDelayMs: 1 } },
		});
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage(
				[
					{
						type: "text",
						text: "partial",
						textSignature: encodeTextSignature("partial-1", "final_answer"),
					},
				],
				{ stopReason: "error", errorMessage: "overloaded_error" },
			),
			fauxAssistantMessage("recovered"),
		]);

		await expect(harness.session.prompt("start")).resolves.toBeUndefined();
		expect(harness.session.exchangeProjection.exchanges[0]?.modelTurns.map((turn) => turn.status)).toEqual([
			"failed",
			"completed",
		]);
		expect(harness.session.exchangeProjection.exchanges[0]?.items).toMatchObject([
			{ type: "narrative", phase: "commentary", status: "interrupted", content: "partial" },
			{ type: "narrative", phase: "final_answer", content: "recovered" },
		]);
	});

	it("interrupts both the active exchange and a follow-up consumed by the aborted run", async () => {
		let toolStarted: (() => void) | undefined;
		const started = new Promise<void>((resolve) => {
			toolStarted = resolve;
		});
		const tool: AgentTool = {
			name: "abortable",
			label: "Abortable",
			description: "Wait until aborted",
			parameters: Type.Object({}),
			execute: async (_toolCallId, _args, signal) => {
				toolStarted?.();
				await new Promise<void>((_resolve, reject) => {
					signal?.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
				});
				return { content: [{ type: "text", text: "unreachable" }], details: {} };
			},
		};
		const harness = await createHarness({ tools: [tool] });
		harnesses.push(harness);
		harness.setResponses([
			stageResponse("I will run the abortable operation.", "Running the abortable operation"),
			fauxAssistantMessage(fauxToolCall("abortable", {}), { stopReason: "toolUse" }),
			fauxAssistantMessage("follow-up result"),
		]);

		const prompt = harness.session.prompt("start");
		await started;
		await harness.session.followUp("next");
		await harness.session.abort();
		await prompt;

		const [abortedExchange, followUpExchange] = harness.session.exchangeProjection.exchanges;
		expect(abortedExchange?.items.filter((item) => item.type === "action")).toMatchObject([
			{ type: "action", status: "cancelled" },
		]);
		expect(abortedExchange?.status).toBe("interrupted");
		expect(followUpExchange?.status).toBe("interrupted");
		expect(followUpExchange?.modelTurns).toMatchObject([{ status: "interrupted" }]);
		expect(followUpExchange?.items).toEqual([]);
	});

	it("persists a custom follow-up as the input of its new exchange", async () => {
		const { tool, release } = createWaitTool();
		const harness = await createHarness({ tools: [tool] });
		harnesses.push(harness);
		harness.setResponses([
			stageResponse("I will wait while the custom follow-up is queued.", "Waiting for custom follow-up work"),
			fauxAssistantMessage(fauxToolCall("wait", {}), { stopReason: "toolUse" }),
			fauxAssistantMessage("initial result"),
			fauxAssistantMessage("custom result"),
		]);

		const toolStarted = waitForToolStart(harness);
		const prompt = harness.session.prompt("initial task");
		await toolStarted;
		await harness.session.sendCustomMessage(
			{ customType: "extension-task", content: "custom task", display: false },
			{ deliverAs: "followUp" },
		);
		release();
		await prompt;

		const entries = harness.sessionManager.getEntries();
		const initial = entries.find(
			(entry) => entry.type === "message" && getMessageText(entry.message) === "initial task",
		);
		const custom = entries.find((entry) => entry.type === "custom_message" && entry.customType === "extension-task");
		const result = entries.find(
			(entry) => entry.type === "message" && getMessageText(entry.message) === "custom result",
		);
		expect(custom).toMatchObject({ delivery: "follow_up" });
		expect(custom?.exchangeId).not.toBe(initial?.exchangeId);
		expect(result?.exchangeId).toBe(custom?.exchangeId);
		expect(result?.modelTurnId).toBe(custom?.modelTurnId);
	});

	it("projects commentary while its model turn is still streaming", async () => {
		const { tool, release } = createWaitTool();
		const harness = await createHarness({ tools: [tool] });
		harnesses.push(harness);
		harness.setResponses([
			stageResponse("Inspecting the request path", "Inspecting the request path"),
			fauxAssistantMessage(fauxToolCall("wait", {}), { stopReason: "toolUse" }),
			fauxAssistantMessage("done"),
		]);
		const streamingContents: string[] = [];
		const unsubscribe = harness.session.subscribeExchangeProjection((projection) => {
			for (const exchange of projection.exchanges) {
				for (const item of exchange.items) {
					if (item.type === "narrative" && item.status === "streaming") streamingContents.push(item.content);
				}
			}
		});

		const toolStarted = waitForToolStart(harness);
		const prompt = harness.session.prompt("start");
		await toolStarted;
		release();
		await prompt;
		unsubscribe();

		expect(streamingContents).toContain("Inspecting the request path");
		expect(harness.session.exchangeProjection.exchanges[0]?.items).toMatchObject([
			{ type: "narrative", phase: "commentary", status: "completed", content: "Inspecting the request path" },
			{ type: "action", status: "completed" },
			{ type: "narrative", phase: "final_answer", status: "completed", content: "done" },
		]);
	});
});
