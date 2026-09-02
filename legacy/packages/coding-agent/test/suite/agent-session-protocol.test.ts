import type { AgentTool } from "@frelion/bone-agent-core";
import { encodeTextSignature, fauxAssistantMessage, fauxToolCall } from "@frelion/bone-ai";
import { Type } from "typebox";
import { afterEach, describe, expect, it } from "vitest";
import {
	classifyAgentProtocolResponse,
	isAgentProtocolCorrectionMessage,
	isAgentProtocolToolResult,
	validateAgentProtocolResponse,
} from "../../src/core/agent-protocol.ts";
import { createHarness, type Harness } from "./harness.ts";

function stage(text: string, title: string) {
	return fauxAssistantMessage(
		[
			{
				type: "text" as const,
				text,
				textSignature: encodeTextSignature(`stage:${title}`, "commentary"),
			},
			fauxToolCall("set_action", { title }),
		],
		{ stopReason: "toolUse" },
	);
}

describe("strict Agent protocol", () => {
	const harnesses: Harness[] = [];

	afterEach(() => {
		while (harnesses.length > 0) harnesses.pop()?.cleanup();
	});

	it("blocks an ordinary tool until the model declares an Action, then executes it once", async () => {
		let executions = 0;
		const probe: AgentTool = {
			name: "probe",
			label: "Probe",
			description: "Record a probe",
			parameters: Type.Object({ value: Type.String() }),
			execute: async (_toolCallId, input) => {
				executions++;
				return { content: [{ type: "text", text: input.value }], details: input };
			},
		};
		const harness = await createHarness({ tools: [probe] });
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage(fauxToolCall("probe", { value: "once" }, { id: "premature-probe" }), {
				stopReason: "toolUse",
			}),
			stage("I will inspect the behavior before reporting back.", "Inspecting protocol behavior"),
			fauxAssistantMessage(fauxToolCall("probe", { value: "once" }, { id: "valid-probe" }), {
				stopReason: "toolUse",
			}),
			fauxAssistantMessage("done"),
		]);

		await harness.session.prompt("probe once");

		expect(executions).toBe(1);
		expect(harness.eventsOfType("tool_execution_start").map((event) => [event.toolCallId, event.toolName])).toEqual([
			[expect.any(String), "set_action"],
			["valid-probe", "probe"],
		]);
		const protocolResults = harness.session.messages.filter(isAgentProtocolToolResult);
		expect(protocolResults).toHaveLength(1);
		expect(protocolResults[0]).toMatchObject({
			toolCallId: "premature-probe",
			details: { internal: { kind: "agent_protocol_error", code: "ACTION_REQUIRED", attempt: 1 } },
		});
		const exchange = harness.session.exchangeProjection.exchanges[0];
		expect(exchange?.status).toBe("completed");
		expect(exchange?.items.filter((item) => item.type === "action")).toMatchObject([
			{ label: "Inspecting protocol behavior", toolCalls: [{ id: "valid-probe", status: "completed" }] },
		]);
	});

	it("rejects set_action mixed with ordinary tools without applying either call", async () => {
		let executions = 0;
		const probe: AgentTool = {
			name: "probe",
			label: "Probe",
			description: "Record a probe",
			parameters: Type.Object({}),
			execute: async () => {
				executions++;
				return { content: [], details: {} };
			},
		};
		const harness = await createHarness({ tools: [probe] });
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage(
				[
					fauxToolCall("set_action", { title: "Invalid mixed Action" }, { id: "mixed-action" }),
					fauxToolCall("probe", {}, { id: "mixed-probe" }),
				],
				{ stopReason: "toolUse" },
			),
			stage("I will retry with an atomic Action declaration.", "Valid atomic Action"),
			fauxAssistantMessage(fauxToolCall("probe", {}, { id: "valid-probe" }), { stopReason: "toolUse" }),
			fauxAssistantMessage("done"),
		]);

		await harness.session.prompt("run probe");

		expect(executions).toBe(1);
		expect(
			harness.session.exchangeProjection.exchanges.flatMap((exchange) =>
				exchange.items.flatMap((item) => (item.type === "action" ? [item.label] : [])),
			),
		).toEqual(["Valid atomic Action"]);
		expect(harness.session.messages.filter(isAgentProtocolToolResult).map((message) => message.toolCallId)).toEqual([
			"mixed-action",
			"mixed-probe",
		]);
	});

	it("corrects an explicit commentary-only stage through a hidden custom message", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage({
				type: "text",
				text: "I will inspect this now.",
				textSignature: encodeTextSignature("commentary-only", "commentary"),
			}),
			stage("I will now declare the inspection Action.", "Inspecting the request"),
			fauxAssistantMessage("done"),
		]);

		await harness.session.prompt("inspect");

		const corrections = harness.session.messages.filter(isAgentProtocolCorrectionMessage);
		expect(corrections).toHaveLength(1);
		expect(corrections[0]).toMatchObject({
			role: "custom",
			customType: "agent-protocol-correction",
			display: false,
			details: { internal: { code: "STAGE_ACTION_REQUIRED", attempt: 1 } },
		});
		expect(
			harness.session.exchangeProjection.exchanges.flatMap((exchange) =>
				exchange.items.filter((item) => item.type === "action"),
			),
		).toHaveLength(1);
	});

	it("fails the Exchange explicitly after three correction attempts", async () => {
		let executions = 0;
		const probe: AgentTool = {
			name: "probe",
			label: "Probe",
			description: "Must never execute",
			parameters: Type.Object({ attempt: Type.Number() }),
			execute: async () => {
				executions++;
				return { content: [], details: {} };
			},
		};
		const harness = await createHarness({ tools: [probe] });
		harnesses.push(harness);
		harness.setResponses(
			[1, 2, 3, 4].map((attempt) =>
				fauxAssistantMessage(fauxToolCall("probe", { attempt }, { id: `invalid-${attempt}` }), {
					stopReason: "toolUse",
				}),
			),
		);

		await harness.session.prompt("keep violating");

		expect(executions).toBe(0);
		expect(harness.eventsOfType("tool_execution_start")).toEqual([]);
		expect(harness.session.messages.filter(isAgentProtocolToolResult)).toHaveLength(4);
		expect(harness.session.messages.at(-1)).toMatchObject({
			role: "assistant",
			stopReason: "error",
			errorMessage: expect.stringContaining("3-attempt protocol correction limit was exceeded"),
		});
		expect(harness.session.exchangeProjection.exchanges[0]).toMatchObject({ status: "failed" });
	});

	it("resets the correction limit after any valid protocol response", async () => {
		const probe: AgentTool = {
			name: "probe",
			label: "Probe",
			description: "Protocol probe",
			parameters: Type.Object({}),
			execute: async () => ({ content: [], details: {} }),
		};
		const harness = await createHarness({ tools: [probe] });
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage(fauxToolCall("probe", {}, { id: "initial-invalid" }), { stopReason: "toolUse" }),
			stage("I will establish a valid Action.", "Inspecting protocol state"),
			fauxAssistantMessage(
				[
					fauxToolCall("set_action", { title: "Invalid mixed switch" }, { id: "mixed-switch" }),
					fauxToolCall("probe", {}, { id: "mixed-probe" }),
				],
				{ stopReason: "toolUse" },
			),
			fauxAssistantMessage(fauxToolCall("set_action", { title: "Valid switch" }), { stopReason: "toolUse" }),
			fauxAssistantMessage("done"),
		]);

		await harness.session.prompt("exercise correction reset");

		expect(
			harness.session.messages.filter(isAgentProtocolToolResult).map((message) => message.details?.internal.attempt),
		).toEqual([1, 1, 1]);
		expect(harness.session.exchangeProjection.exchanges[0]?.status).toBe("completed");
	});
});

describe("Agent protocol grammar", () => {
	it("accepts the four response forms", () => {
		expect(validateAgentProtocolResponse(stage("Starting inspection.", "Inspecting"), false)).toBeUndefined();
		expect(
			validateAgentProtocolResponse(
				fauxAssistantMessage(fauxToolCall("set_action", { title: "Implementing" }), { stopReason: "toolUse" }),
				true,
			),
		).toBeUndefined();
		expect(
			validateAgentProtocolResponse(
				fauxAssistantMessage(fauxToolCall("read", { path: "x" }), { stopReason: "toolUse" }),
				true,
			),
		).toBeUndefined();
		expect(validateAgentProtocolResponse(fauxAssistantMessage("done"), true)).toBeUndefined();
	});

	it("reports stable error codes for invalid response shapes", () => {
		const cases = [
			[
				fauxAssistantMessage(fauxToolCall("read", { path: "x" }), { stopReason: "toolUse" }),
				false,
				"ACTION_REQUIRED",
			],
			[
				fauxAssistantMessage(fauxToolCall("set_action", { title: "Inspecting" }), { stopReason: "toolUse" }),
				false,
				"STAGE_UPDATE_REQUIRED",
			],
			[
				fauxAssistantMessage([
					fauxToolCall("set_action", { title: "One" }),
					fauxToolCall("set_action", { title: "Two" }),
				]),
				true,
				"MULTIPLE_ACTION_DECLARATIONS",
			],
		] as const;
		for (const [message, active, code] of cases) {
			expect(validateAgentProtocolResponse(message, active)?.code).toBe(code);
		}
	});

	it("keeps unphased and explicit final text in response order", () => {
		const response = classifyAgentProtocolResponse(
			fauxAssistantMessage([
				{ type: "text", text: "first" },
				{
					type: "text",
					text: "second",
					textSignature: encodeTextSignature("final", "final_answer"),
				},
			]),
			{ hasActiveAction: true },
		);

		expect(response.disposition).toBe("final");
		expect(response.finalAnswer).toBe("first\nsecond");
	});
});
