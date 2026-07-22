import { describe, expect, it } from "vitest";
import { forgeEvalCases } from "../evals/forge/cases.ts";
import { liveForgeEvalCases } from "../evals/forge/live-cases.ts";
import { parseLiveForgeEvalArgs, runLiveForgeEvalCase } from "../evals/forge/live-run.ts";
import { renderForgeEvalReport } from "../evals/forge/report.ts";
import { runForgeEvalCase, runForgeEvals } from "../evals/forge/run.ts";
import { createScriptedStream, forgeEvalModel } from "../evals/forge/scripted-stream.ts";

describe("scripted Forge protocol evaluation", () => {
	it("runs the high-value offline cases through the real Agent loop", async () => {
		const report = await runForgeEvals();

		expect(report.mode).toBe("scripted");
		expect(report.summary).toMatchObject({ total: forgeEvalCases.length, passed: forgeEvalCases.length, failed: 0 });
		expect(report.cases.every((result) => result.trace.modelContexts.length > 0)).toBe(true);
		expect(report.cases.every((result) => result.trace.finalAssistantText.length > 0)).toBe(true);
	});

	it("records a corrected validation call without sending the invalid payload to service", async () => {
		const result = await runForgeEvalCase(
			forgeEvalCases.find((forgeCase) => forgeCase.id === "query.validation-correction")!,
		);

		expect(result.passed).toBe(true);
		expect(result.trace.serviceCalls).toHaveLength(1);
		expect(result.trace.toolResults.some((toolResult) => toolResult.isError)).toBe(true);
	});

	it("fails a case when the scripted model makes an unexpected extra service call", async () => {
		const base = forgeEvalCases.find((forgeCase) => forgeCase.id === "query.issue.list-success")!;
		const result = await runForgeEvalCase({
			...base,
			id: "test.unexpected-extra-call",
			steps: [base.steps[0], base.steps[0], { text: "done" }],
		});

		expect(result.passed).toBe(false);
		expect(result.trace.unexpectedServiceCalls).toHaveLength(1);
	});

	it("escapes dynamic values in the HTML report", () => {
		const html = renderForgeEvalReport({
			schemaVersion: 1,
			mode: "scripted",
			generatedAt: "<script>alert(1)</script>",
			summary: { total: 1, passed: 0, failed: 1, protocolPassRate: 0 },
			cases: [
				{
					id: "<case>",
					category: "query",
					passed: false,
					assertions: [{ name: "<bad>", passed: false, detail: "</li><script>bad()</script>" }],
					trace: {
						modelSteps: 0,
						modelContexts: [],
						serviceCalls: [],
						unexpectedServiceCalls: [],
						toolResults: [],
						finalAssistantText: '<img src=x onerror="bad()">',
						completed: false,
						scriptExhausted: false,
					},
				},
			],
		});

		expect(html).not.toContain("<script>alert(1)</script>");
		expect(html).toContain("&lt;script&gt;alert(1)&lt;/script&gt;");
		expect(html).toContain("&lt;/li&gt;&lt;script&gt;bad()&lt;/script&gt;");
		expect(html).toContain("&lt;img src=x onerror=\\&quot;bad()\\&quot;&gt;");
		expect(html).not.toContain('<img src=x onerror="bad()">');
	});

	it("parses live evaluation limits without contacting a model", () => {
		expect(
			parseLiveForgeEvalArgs([
				"--model",
				"openai/gpt-4o",
				"--runs",
				"3",
				"--max-turns",
				"4",
				"--timeout-ms",
				"5000",
			]),
		).toMatchObject({
			model: "openai/gpt-4o",
			repetitions: 3,
			maxTurns: 4,
			timeoutMs: 5000,
		});
	});

	it("scores a live trace through the real Agent loop with an injected stream", async () => {
		const forgeCase = liveForgeEvalCases[0];
		const scripted = createScriptedStream([
			{ toolCalls: [{ name: "forge_query", args: { operation: "list", resource: "issue", state: "open" } }] },
			{ text: "Login fails" },
		]);
		const result = await runLiveForgeEvalCase(forgeCase, 1, {
			model: forgeEvalModel,
			streamFn: async (model, context, options) => scripted.streamFn(model, context, options),
			maxTurns: 4,
			timeoutMs: 5_000,
		});
		expect(result.passed).toBe(true);
		expect(result.metrics.firstToolSelectionCorrect).toBe(true);
		expect(result.trace.model).toBe("forge-eval/forge-eval-scripted");
	});

	it("does not call the provider beyond the live model-turn limit", async () => {
		const scripted = createScriptedStream([
			{ toolCalls: [{ name: "forge_query", args: { operation: "list", resource: "issue", state: "open" } }] },
			{ text: "This second provider response must not be requested." },
		]);
		await runLiveForgeEvalCase(liveForgeEvalCases[0], 1, {
			model: forgeEvalModel,
			streamFn: async (model, context, options) => scripted.streamFn(model, context, options),
			maxTurns: 1,
			timeoutMs: 5_000,
		});
		expect(scripted.modelStepCount).toBe(1);
	});
});
