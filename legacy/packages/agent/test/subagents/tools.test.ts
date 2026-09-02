import { describe, expect, it, vi } from "vitest";
import { InProcessSubagentRuntime } from "../../src/subagents/in-process-runtime.ts";
import { createSubagentTools } from "../../src/subagents/tools.ts";
import type { SubagentHandoff } from "../../src/subagents/types.ts";
import { AgentToolError } from "../../src/types.ts";

function handoff(summary: string): SubagentHandoff {
	return { status: "completed", summary };
}

describe("createSubagentTools", () => {
	it("exposes handles and bounded handoffs without child event history", async () => {
		const inputs: string[] = [];
		const runtime = new InProcessSubagentRuntime({
			createId: () => "tool-child",
			createSession: () => ({
				run: async (input) => {
					inputs.push(input.text);
					return handoff(input.kind === "delegation" ? "initial" : "answer");
				},
				abort: async () => {},
			}),
		});
		const tools = createSubagentTools(runtime);

		const delegated = await tools.delegateStage.execute("call-1", {
			objective: "Inspect the parser",
			scope: "conversation",
			contextRefs: ["src/parser.ts"],
		});
		expect(delegated.details).toMatchObject({
			id: "tool-child",
			scope: "conversation",
		});

		const waited = await tools.waitAgent.execute("call-2", { agentRef: "tool-child" });
		expect(waited.details).toEqual(handoff("initial"));
		expect(waited.content).toEqual([{ type: "text", text: JSON.stringify(handoff("initial")) }]);

		const asked = await tools.askAgent.execute("call-3", {
			agentRef: "tool-child",
			question: "Why?",
		});
		expect(asked.details).toEqual(handoff("answer"));
		expect(inputs).toEqual(["Inspect the parser", "Why?"]);

		await tools.closeAgent.execute("call-4", { agentRef: "tool-child" });
		expect(runtime.get("tool-child")?.status).toBe("closed");
	});

	it("maps runtime failures to model-facing structured tool errors", async () => {
		const tools = createSubagentTools(
			new InProcessSubagentRuntime({
				createSession: () => ({
					run: async () => handoff("unused"),
					abort: async () => {},
				}),
			}),
		);

		const result = tools.waitAgent.execute("call", { agentRef: "missing" }).catch((error: unknown) => error);
		await expect(result).resolves.toBeInstanceOf(AgentToolError);
		await expect(result).resolves.toMatchObject({
			code: "subagent_unknown_agent",
			retryable: false,
		});
	});

	it("propagates tool cancellation to a waiting child run", async () => {
		let aborted = false;
		const runtime = new InProcessSubagentRuntime({
			createId: () => "abortable-tool-child",
			createSession: () => ({
				run: async (_input, signal) =>
					await new Promise<SubagentHandoff>((_resolve, reject) => {
						signal?.addEventListener("abort", () => reject(new Error("aborted")), { once: true });
					}),
				abort: async () => {
					aborted = true;
				},
			}),
		});
		const tools = createSubagentTools(runtime);
		await tools.delegateStage.execute("delegate", { objective: "Wait forever" });
		await Promise.resolve();
		const controller = new AbortController();
		const waiting = tools.waitAgent.execute("wait", { agentRef: "abortable-tool-child" }, controller.signal);

		controller.abort("Parent aborted");

		await expect(waiting).rejects.toMatchObject({ code: "subagent_cancelled" });
		await vi.waitFor(() => expect(aborted).toBe(true));
	});

	it("drains bounded child messages through read_agent_messages", async () => {
		let rejectRun!: (error: Error) => void;
		const runGate = new Promise<SubagentHandoff>((_resolve, reject) => {
			rejectRun = reject;
		});
		const runtime = new InProcessSubagentRuntime({
			createId: () => "message-tool-child",
			createSession: ({ publishYield }) => ({
				run: async () => {
					publishYield({ kind: "progress", message: "Scanning" });
					publishYield({ kind: "finding", message: "Found mismatch", artifactRefs: ["src/parser.ts"] });
					return await runGate;
				},
				abort: async () => rejectRun(new Error("aborted")),
			}),
		});
		const tools = createSubagentTools(runtime);
		await tools.delegateStage.execute("delegate", { objective: "Investigate" });
		await vi.waitFor(() => expect(runtime.list()[0]?.status).toBe("running"));

		const result = await tools.readAgentMessages.execute("read", { agentRef: "message-tool-child" });

		expect(result.details.map(({ sequence, kind, message }) => ({ sequence, kind, message }))).toEqual([
			{ sequence: 1, kind: "progress", message: "Scanning" },
			{ sequence: 2, kind: "finding", message: "Found mismatch" },
		]);
		expect(result.details[1]?.artifactRefs).toEqual(["src/parser.ts"]);
		await expect(
			tools.readAgentMessages.execute("read-again", { agentRef: "message-tool-child" }),
		).resolves.toMatchObject({ details: [] });

		await runtime.cancel("message-tool-child");
	});

	it("maps read_agent_messages aborts and unknown handles through structured tool errors", async () => {
		const runtime = new InProcessSubagentRuntime({
			createSession: () => ({
				run: async () => handoff("unused"),
				abort: async () => {},
			}),
		});
		const tools = createSubagentTools(runtime);
		const controller = new AbortController();
		controller.abort("Parent stopped");

		await expect(
			tools.readAgentMessages.execute("aborted", { agentRef: "missing" }, controller.signal),
		).rejects.toMatchObject({ code: "subagent_cancelled", retryable: false });
		await expect(tools.readAgentMessages.execute("unknown", { agentRef: "missing" })).rejects.toMatchObject({
			code: "subagent_unknown_agent",
			retryable: false,
		});
	});
});
