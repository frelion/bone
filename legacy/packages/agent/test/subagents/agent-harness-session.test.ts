import type { AssistantMessage } from "@frelion/bone-ai";
import { describe, expect, it } from "vitest";
import { createAgentHarnessSubagentSession } from "../../src/subagents/agent-harness-session.ts";

function assistantMessage(text: string, stopReason: AssistantMessage["stopReason"] = "stop"): AssistantMessage {
	return {
		role: "assistant",
		content: [{ type: "text", text }],
		api: "test",
		provider: "test",
		model: "test",
		stopReason,
		timestamp: 1,
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
	};
}

describe("createAgentHarnessSubagentSession", () => {
	it("maps the assistant response to a bounded handoff", async () => {
		const prompts: string[] = [];
		const session = createAgentHarnessSubagentSession({
			prompt: async (text) => {
				prompts.push(text);
				return assistantMessage("  concise conclusion  ");
			},
			abort: async () => {},
		});

		await expect(session.run({ kind: "delegation", text: "Investigate" })).resolves.toEqual({
			status: "completed",
			summary: "concise conclusion",
		});
		expect(prompts).toEqual(["Investigate"]);
	});

	it("supports application-defined structured handoffs and delegates abort", async () => {
		let aborted = false;
		const session = createAgentHarnessSubagentSession(
			{
				prompt: async () => assistantMessage("raw"),
				abort: async () => {
					aborted = true;
				},
			},
			(_message, input) => ({
				status: "partial",
				summary: `Mapped ${input.kind}`,
				changedFiles: ["src/parser.ts"],
			}),
		);

		await expect(session.run({ kind: "question", text: "Why?" })).resolves.toEqual({
			status: "partial",
			summary: "Mapped question",
			changedFiles: ["src/parser.ts"],
		});
		await session.abort("stop");
		expect(aborted).toBe(true);
	});

	it("maps truncated and failed assistant responses without losing diagnostics", async () => {
		const lengthSession = createAgentHarnessSubagentSession({
			prompt: async () => assistantMessage("truncated", "length"),
			abort: async () => {},
		});
		const failedMessage = assistantMessage("", "error");
		failedMessage.errorMessage = "Provider unavailable";
		const failedSession = createAgentHarnessSubagentSession({
			prompt: async () => failedMessage,
			abort: async () => {},
		});

		await expect(lengthSession.run({ kind: "delegation", text: "Work" })).resolves.toEqual({
			status: "partial",
			summary: "truncated",
		});
		await expect(failedSession.run({ kind: "delegation", text: "Work" })).resolves.toEqual({
			status: "failed",
			summary: "Provider unavailable",
		});
	});
});
