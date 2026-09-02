import { InProcessSubagentRuntime } from "@frelion/bone-agent-core";
import { describe, expect, it } from "vitest";
import { CodingSubagentManager } from "../src/core/subagents/index.ts";

describe("CodingSubagentManager yield projection", () => {
	it("retains pre-registration yields and marks drained messages read without dropping history", async () => {
		let publishYield: ((input: { kind: "finding"; message: string }) => void) | undefined;
		let finishRun: (() => void) | undefined;
		const runFinished = new Promise<void>((resolve) => {
			finishRun = resolve;
		});
		const runtime = new InProcessSubagentRuntime({
			createId: () => "child-race",
			maxClosedSessions: 0,
			createSession: (context) => {
				publishYield = context.publishYield;
				context.publishYield({
					kind: "finding",
					message: "Published before the parent registers the delegated handle.",
				});
				return {
					run: async () => {
						await runFinished;
						return { status: "completed", summary: "Done" };
					},
					abort: async () => {},
				};
			},
		});
		const manager = new CodingSubagentManager({ runtime });
		const handle = await runtime.delegate({ objective: "Exercise registration ordering" });
		await Promise.resolve();
		manager.register(handle, {
			exchangeId: "exchange-race",
			actionId: "action-race",
			toolCallId: "delegate-race",
		});
		await expect.poll(() => runtime.get(handle.id)?.status).toBe("running");
		publishYield?.({ kind: "finding", message: "Second message" });
		publishYield?.({ kind: "finding", message: "Third message" });

		expect(manager.projection.executions[0]).toMatchObject({
			agentRef: "child-race",
			unreadYieldCount: 3,
			yields: [
				{
					sequence: 1,
					kind: "finding",
					message: "Published before the parent registers the delegated handle.",
				},
				{ sequence: 2, message: "Second message" },
				{ sequence: 3, message: "Third message" },
			],
		});
		const projectedYields = manager.projection.executions[0]?.yields ?? [];
		manager.recordYieldsRead(handle.id, projectedYields.slice(2));
		manager.recordYieldsRead(handle.id, projectedYields.slice(0, 2));
		expect(manager.projection.executions[0]?.unreadYieldCount).toBe(0);
		const drained = runtime.drainYields(handle.id);
		manager.recordYieldsRead(handle.id, drained);
		finishRun?.();
		await runtime.wait(handle.id);
		await manager.closeAll();
		expect(manager.projection.executions).toEqual([]);
	});
});
