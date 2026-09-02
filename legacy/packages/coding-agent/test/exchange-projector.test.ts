import { describe, expect, it } from "vitest";
import {
	createExchangeProjection,
	ExchangeProjectionError,
	getActiveActions,
	projectExchangeEvent,
	shouldShowWorking,
} from "../src/core/exchange/index.ts";
import type { ExchangeProjection, ExchangeProjectorEvent } from "../src/core/exchange/types.ts";

function project(events: readonly ExchangeProjectorEvent[]): ExchangeProjection {
	return events.reduce(projectExchangeEvent, createExchangeProjection("session-1"));
}

function startExchange(): ExchangeProjectorEvent {
	return {
		type: "exchange_started",
		exchangeId: "exchange-1",
		input: { id: "input-1", delivery: "prompt", content: "Fix Working" },
		at: 1,
	};
}

describe("Exchange projector", () => {
	it("projects a semantic action with child tool calls in timeline order", () => {
		let state = project([
			startExchange(),
			{ type: "model_turn_started", exchangeId: "exchange-1", modelTurnId: "turn-1", at: 2 },
			{
				type: "narrative_started",
				exchangeId: "exchange-1",
				narrativeId: "commentary-1",
				modelTurnId: "turn-1",
				phase: "commentary",
				content: "I will inspect ",
				at: 3,
			},
			{
				type: "narrative_delta",
				exchangeId: "exchange-1",
				narrativeId: "commentary-1",
				delta: "the state machine.",
				at: 4,
			},
			{
				type: "narrative_completed",
				exchangeId: "exchange-1",
				narrativeId: "commentary-1",
				at: 5,
			},
			{
				type: "action_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				kind: "investigation",
				label: "Inspect Working lifecycle",
				at: 6,
			},
			{
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				modelTurnId: "turn-1",
				toolName: "search",
				arguments: { query: "Working" },
				at: 7,
			},
		]);

		expect(shouldShowWorking(state.exchanges[0]!)).toBe(false);
		expect(getActiveActions(state.exchanges[0]!).map((action) => action.id)).toEqual(["action-1"]);

		state = projectExchangeEvent(state, {
			type: "action_tool_call_completed",
			exchangeId: "exchange-1",
			actionId: "action-1",
			toolCallId: "call-1",
			result: { matches: 2 },
			at: 8,
		});
		expect(shouldShowWorking(state.exchanges[0]!)).toBe(false);

		state = projectExchangeEvent(state, {
			type: "action_completed",
			exchangeId: "exchange-1",
			actionId: "action-1",
			outcome: { inspected: true },
			at: 9,
		});
		expect(shouldShowWorking(state.exchanges[0]!)).toBe(true);
		expect(state.exchanges[0]!.items).toMatchObject([
			{
				id: "commentary-1",
				phase: "commentary",
				status: "completed",
				content: "I will inspect the state machine.",
				sequence: 0,
			},
			{
				id: "action-1",
				kind: "investigation",
				status: "completed",
				modelTurnIds: ["turn-1"],
				outcome: { inspected: true },
				sequence: 1,
				toolCalls: [
					{
						id: "call-1",
						modelTurnId: "turn-1",
						toolName: "search",
						status: "completed",
						arguments: { query: "Working" },
						result: { matches: 2 },
						sequence: 0,
					},
				],
			},
		]);

		state = project([
			startExchange(),
			{ type: "model_turn_started", exchangeId: "exchange-1", modelTurnId: "turn-1", at: 2 },
			{ type: "model_turn_completed", exchangeId: "exchange-1", modelTurnId: "turn-1", at: 3 },
			{ type: "model_turn_started", exchangeId: "exchange-1", modelTurnId: "turn-2", at: 4 },
			{
				type: "narrative_started",
				exchangeId: "exchange-1",
				narrativeId: "final-1",
				modelTurnId: "turn-2",
				phase: "final_answer",
				content: "Fixed.",
				at: 5,
			},
			{ type: "narrative_completed", exchangeId: "exchange-1", narrativeId: "final-1", at: 6 },
			{ type: "model_turn_completed", exchangeId: "exchange-1", modelTurnId: "turn-2", at: 7 },
			{ type: "exchange_completed", exchangeId: "exchange-1", at: 8 },
		]);
		expect(state.exchanges[0]).toMatchObject({ status: "completed", completedAt: 8 });
		expect(shouldShowWorking(state.exchanges[0]!)).toBe(false);
	});

	it("keeps one semantic action active across multiple model turns", () => {
		let state = project([
			startExchange(),
			{ type: "model_turn_started", exchangeId: "exchange-1", modelTurnId: "turn-1", at: 2 },
			{
				type: "action_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				kind: "implementation",
				label: "Implement the lifecycle",
				at: 3,
			},
			{
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				modelTurnId: "turn-1",
				toolName: "read",
				arguments: { path: "projector.ts" },
				at: 4,
			},
			{
				type: "action_tool_call_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				at: 5,
			},
			{ type: "model_turn_completed", exchangeId: "exchange-1", modelTurnId: "turn-1", at: 6 },
		]);

		expect(getActiveActions(state.exchanges[0]!)).toHaveLength(1);
		expect(state.exchanges[0]!.items[0]).toMatchObject({ status: "in_progress" });

		state = [
			{ type: "model_turn_started", exchangeId: "exchange-1", modelTurnId: "turn-2", at: 7 },
			{
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-2",
				modelTurnId: "turn-2",
				toolName: "edit",
				arguments: { path: "projector.ts" },
				at: 8,
			},
			{
				type: "action_tool_call_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-2",
				at: 9,
			},
			{ type: "model_turn_completed", exchangeId: "exchange-1", modelTurnId: "turn-2", at: 10 },
			{
				type: "action_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				at: 11,
			},
		].reduce(projectExchangeEvent, state);

		expect(state.exchanges[0]!.items[0]).toMatchObject({
			modelTurnIds: ["turn-1", "turn-2"],
			status: "completed",
			toolCalls: [{ id: "call-1" }, { id: "call-2" }],
		});
	});

	it("supports parallel child calls and restores Working only after semantic completion", () => {
		let state = project([
			startExchange(),
			{ type: "model_turn_started", exchangeId: "exchange-1", modelTurnId: "turn-1", at: 2 },
			{
				type: "action_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				kind: "investigation",
				label: "Inspect files",
				at: 3,
			},
			{
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				modelTurnId: "turn-1",
				toolName: "read",
				arguments: {},
				at: 4,
			},
			{
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-2",
				modelTurnId: "turn-1",
				toolName: "search",
				arguments: {},
				at: 5,
			},
		]);

		for (const [toolCallId, at] of [
			["call-1", 6],
			["call-2", 7],
		] as const) {
			state = projectExchangeEvent(state, {
				type: "action_tool_call_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId,
				at,
			});
			expect(shouldShowWorking(state.exchanges[0]!)).toBe(false);
		}

		state = projectExchangeEvent(state, {
			type: "action_completed",
			exchangeId: "exchange-1",
			actionId: "action-1",
			at: 8,
		});
		expect(shouldShowWorking(state.exchanges[0]!)).toBe(true);
	});

	it("allows recovery after a failed tool call in a later turn", () => {
		const state = project([
			startExchange(),
			{ type: "model_turn_started", exchangeId: "exchange-1", modelTurnId: "turn-1", at: 2 },
			{
				type: "action_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				kind: "verification",
				label: "Verify changes",
				at: 3,
			},
			{
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				modelTurnId: "turn-1",
				toolName: "test",
				arguments: {},
				at: 4,
			},
			{
				type: "action_tool_call_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				status: "failed",
				error: "Test failed",
				at: 5,
			},
			{ type: "model_turn_completed", exchangeId: "exchange-1", modelTurnId: "turn-1", at: 6 },
			{ type: "model_turn_started", exchangeId: "exchange-1", modelTurnId: "turn-2", at: 7 },
			{
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-2",
				modelTurnId: "turn-2",
				toolName: "test",
				arguments: {},
				at: 8,
			},
			{
				type: "action_tool_call_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-2",
				result: { passed: true },
				at: 9,
			},
			{ type: "model_turn_completed", exchangeId: "exchange-1", modelTurnId: "turn-2", at: 10 },
			{
				type: "action_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				outcome: { verified: true },
				at: 11,
			},
		]);

		expect(state.exchanges[0]!.items[0]).toMatchObject({
			status: "completed",
			toolCalls: [{ status: "failed" }, { status: "completed" }],
		});
	});

	it("rejects invalid semantic action and child tool call transitions", () => {
		let state = project([
			startExchange(),
			{ type: "model_turn_started", exchangeId: "exchange-1", modelTurnId: "turn-1", at: 2 },
			{
				type: "action_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				kind: "test",
				label: "Run tests",
				at: 3,
			},
		]);

		expect(() =>
			projectExchangeEvent(state, {
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "missing",
				toolCallId: "call-x",
				modelTurnId: "turn-1",
				toolName: "test",
				arguments: {},
				at: 4,
			}),
		).toThrow("Unknown action");
		expect(() =>
			projectExchangeEvent(state, {
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-x",
				modelTurnId: "missing",
				toolName: "test",
				arguments: {},
				at: 4,
			}),
		).toThrow("Unknown model turn");
		expect(() =>
			projectExchangeEvent(state, {
				type: "action_tool_call_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "missing",
				at: 4,
			}),
		).toThrow("Unknown tool call");

		state = projectExchangeEvent(state, {
			type: "action_tool_call_started",
			exchangeId: "exchange-1",
			actionId: "action-1",
			toolCallId: "call-1",
			modelTurnId: "turn-1",
			toolName: "test",
			arguments: {},
			at: 4,
		});
		expect(() =>
			projectExchangeEvent(state, {
				type: "model_turn_completed",
				exchangeId: "exchange-1",
				modelTurnId: "turn-1",
				at: 5,
			}),
		).toThrow("active tool call");
		expect(() =>
			projectExchangeEvent(state, {
				type: "action_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				at: 5,
			}),
		).toThrow("active tool call");
		expect(() =>
			projectExchangeEvent(state, {
				type: "narrative_started",
				exchangeId: "exchange-1",
				narrativeId: "final-1",
				modelTurnId: "turn-1",
				phase: "final_answer",
				at: 5,
			}),
		).toThrow("active action");
		expect(() =>
			projectExchangeEvent(state, {
				type: "exchange_completed",
				exchangeId: "exchange-1",
				at: 5,
			}),
		).toThrow("running model turn");

		state = projectExchangeEvent(state, {
			type: "action_tool_call_completed",
			exchangeId: "exchange-1",
			actionId: "action-1",
			toolCallId: "call-1",
			at: 6,
		});
		expect(() =>
			projectExchangeEvent(state, {
				type: "action_tool_call_updated",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				progress: {},
				at: 7,
			}),
		).toThrow("Tool call is not in progress");
		expect(() =>
			projectExchangeEvent(state, {
				type: "action_tool_call_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				at: 7,
			}),
		).toThrow("Tool call is not in progress");

		state = projectExchangeEvent(state, {
			type: "action_started",
			exchangeId: "exchange-1",
			actionId: "action-2",
			kind: "test",
			label: "Retry tests",
			at: 7,
		});
		expect(() =>
			projectExchangeEvent(state, {
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-2",
				toolCallId: "call-1",
				modelTurnId: "turn-1",
				toolName: "test",
				arguments: {},
				at: 8,
			}),
		).toThrow("Tool call already exists");

		state = projectExchangeEvent(state, {
			type: "action_completed",
			exchangeId: "exchange-1",
			actionId: "action-1",
			at: 8,
		});
		expect(() =>
			projectExchangeEvent(state, {
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-2",
				modelTurnId: "turn-1",
				toolName: "test",
				arguments: {},
				at: 9,
			}),
		).toThrow("Action is not in progress");
	});

	it("requires semantic actions to finish before final narrative and exchange completion", () => {
		let state = project([
			startExchange(),
			{
				type: "action_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				kind: "planning",
				label: "Plan changes",
				at: 2,
			},
		]);

		expect(() =>
			projectExchangeEvent(state, {
				type: "narrative_started",
				exchangeId: "exchange-1",
				narrativeId: "final-1",
				phase: "final_answer",
				at: 3,
			}),
		).toThrow("active action");
		expect(() =>
			projectExchangeEvent(state, {
				type: "exchange_completed",
				exchangeId: "exchange-1",
				at: 3,
			}),
		).toThrow("active action");

		state = projectExchangeEvent(state, {
			type: "action_completed",
			exchangeId: "exchange-1",
			actionId: "action-1",
			at: 4,
		});
		state = projectExchangeEvent(state, {
			type: "narrative_started",
			exchangeId: "exchange-1",
			narrativeId: "final-1",
			phase: "final_answer",
			at: 5,
		});
		expect(() =>
			projectExchangeEvent(state, {
				type: "action_started",
				exchangeId: "exchange-1",
				actionId: "action-2",
				kind: "work",
				label: "More work",
				at: 6,
			}),
		).toThrow("after final answer");
	});

	it("preserves child progress and separate failed outcomes", () => {
		const state = project([
			startExchange(),
			{ type: "model_turn_started", exchangeId: "exchange-1", modelTurnId: "turn-1", at: 2 },
			{
				type: "action_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				kind: "verification",
				label: "Run interaction tests",
				at: 3,
			},
			{
				type: "action_tool_call_started",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				modelTurnId: "turn-1",
				toolName: "test",
				arguments: { command: "vitest" },
				at: 4,
			},
			{
				type: "action_tool_call_updated",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				progress: { output: "1 failed" },
				at: 5,
			},
			{
				type: "action_tool_call_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				toolCallId: "call-1",
				status: "failed",
				error: "Test failed",
				at: 6,
			},
			{
				type: "action_completed",
				exchangeId: "exchange-1",
				actionId: "action-1",
				status: "failed",
				outcome: { passed: 0 },
				error: "Verification failed",
				at: 7,
			},
			{
				type: "model_turn_completed",
				exchangeId: "exchange-1",
				modelTurnId: "turn-1",
				status: "interrupted",
				at: 8,
			},
			{ type: "exchange_completed", exchangeId: "exchange-1", status: "interrupted", at: 9 },
		]);

		expect(state.exchanges[0]).toMatchObject({
			status: "interrupted",
			modelTurns: [{ status: "interrupted" }],
			items: [
				{
					type: "action",
					status: "failed",
					outcome: { passed: 0 },
					error: "Verification failed",
					toolCalls: [
						{
							status: "failed",
							progress: { output: "1 failed" },
							error: "Test failed",
						},
					],
				},
			],
		});
	});

	it("keeps commentary separate from Working and preserves Exchange input semantics", () => {
		const state = project([
			startExchange(),
			{
				type: "exchange_input_added",
				exchangeId: "exchange-1",
				input: { id: "input-2", delivery: "steer", content: "Also cover retries" },
				at: 2,
			},
			{
				type: "narrative_started",
				exchangeId: "exchange-1",
				narrativeId: "commentary-1",
				phase: "commentary",
				content: "I am narrowing down the owner.",
				at: 3,
			},
		]);

		expect(state.exchanges[0]!.inputs.map((input) => input.delivery)).toEqual(["prompt", "steer"]);
		expect(shouldShowWorking(state.exchanges[0]!)).toBe(true);
		expect(() =>
			projectExchangeEvent(state, {
				type: "exchange_started",
				exchangeId: "exchange-2",
				input: { id: "input-3", delivery: "follow_up", content: "Now add tests" },
				at: 4,
			}),
		).toThrow(ExchangeProjectionError);
	});
});
