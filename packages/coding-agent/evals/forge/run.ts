import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { agentLoop, type AgentContext, type AgentEvent, type AgentMessage } from "../../../agent/src/index.ts";
import { createForgeToolDefinitions } from "../../src/core/forge/tools.ts";
import { wrapToolDefinition } from "../../src/core/tools/tool-definition-wrapper.ts";
import { forgeEvalCases } from "./cases.ts";
import { FakeForgeService } from "./fake-forge-service.ts";
import { identityEvalConverter, createScriptedStream, forgeEvalModel } from "./scripted-stream.ts";
import type { EvalTrace, ForgeEvalCase, ForgeEvalCaseResult, ForgeEvalReport } from "./types.ts";
import { renderForgeEvalReport } from "./report.ts";

const DEFAULT_OUTPUT = resolve(dirname(fileURLToPath(import.meta.url)), "../../../../.artifacts/forge-eval");

function userMessage(content: string): AgentMessage {
	return { role: "user", content, timestamp: Date.now() };
}

function textContent(result: any): string {
	return result?.content
		?.filter((content: any) => content.type === "text")
		.map((content: any) => content.text)
		.join("\n") ?? "";
}

function errorCode(text: string): string | undefined {
	try {
		const parsed = JSON.parse(text) as { error?: { code?: unknown } };
		return typeof parsed.error?.code === "string" ? parsed.error.code : undefined;
	} catch {
		return undefined;
	}
}

function makeAssertions(
	forgeCase: ForgeEvalCase,
	trace: EvalTrace,
	service: FakeForgeService,
): ForgeEvalCaseResult["assertions"] {
	const expected = forgeCase.expectations;
	const assertions: ForgeEvalCaseResult["assertions"] = [];
	const add = (name: string, passed: boolean, detail?: string) => assertions.push({ name, passed, detail });
	const errorTexts = trace.toolResults.filter((result) => result.isError).map((result) => result.text);
	const allVisibleText = [...trace.toolResults.map((result) => result.text), trace.finalAssistantText].join("\n");

	add("task completes without exhausting scripted model", expected.mustComplete === !trace.scriptExhausted && trace.completed);
	add("fake service receives no unexpected calls", trace.unexpectedServiceCalls.length === 0, JSON.stringify(trace.unexpectedServiceCalls));
	add(
		"service call count matches",
		service.calls.length === expected.expectedServiceCalls,
		`expected ${expected.expectedServiceCalls}, got ${service.calls.length}`,
	);
	if (expected.expectedServiceTools) {
		add(
			"service tool trace matches",
			JSON.stringify(service.calls.map((call) => call.toolName)) === JSON.stringify(expected.expectedServiceTools),
			`expected ${JSON.stringify(expected.expectedServiceTools)}, got ${JSON.stringify(service.calls.map((call) => call.toolName))}`,
		);
	}
	if (expected.expectedErrors) {
		for (const expectedError of expected.expectedErrors) {
			add(
				`error trace contains ${expectedError}`,
				errorTexts.some((text) => text.includes(expectedError) || errorCode(text) === expectedError),
				`errors: ${errorTexts.map((text) => errorCode(text) ?? text.slice(0, 120)).join(", ")}`,
			);
		}
	}
	if (expected.expectedDuplicateFailures !== undefined) {
		const duplicateFailures = errorTexts.filter((text) => errorCode(text) === "duplicate_failed_call").length;
		add(
			"duplicate failure count matches",
			duplicateFailures === expected.expectedDuplicateFailures,
			`expected ${expected.expectedDuplicateFailures}, got ${duplicateFailures}`,
		);
	}
	if (expected.expectedContextIncludesToolResult !== undefined) {
		add(
			"model receives a tool result context",
			trace.modelContexts.some((context) => context.hasToolResult) === expected.expectedContextIncludesToolResult,
		);
	}
	if (expected.maxContextBytes !== undefined) {
		const maximum = Math.max(0, ...trace.modelContexts.map((context) => context.bytes));
		add("context stays within budget", maximum <= expected.maxContextBytes, `maximum ${maximum} bytes`);
	}
	if (expected.maxToolResultBytes !== undefined) {
		const maximum = Math.max(0, ...trace.toolResults.map((result) => result.bytes));
		add("tool results stay within budget", maximum <= expected.maxToolResultBytes, `maximum ${maximum} bytes`);
	}
	for (const sentinel of expected.mustNotContain ?? []) {
		add(`visible output omits ${sentinel}`, !allVisibleText.includes(sentinel));
	}
	return assertions;
}

export async function runForgeEvalCase(forgeCase: ForgeEvalCase): Promise<ForgeEvalCaseResult> {
	const service = new FakeForgeService(forgeCase.service);
	const definitions = createForgeToolDefinitions({ cwd: "/forge-eval", service });
	const tools = Object.values(definitions).map((definition) => wrapToolDefinition(definition));
	const context: AgentContext = { systemPrompt: "You are running a deterministic Forge protocol evaluation.", messages: [], tools };
	const scripted = createScriptedStream(forgeCase.steps);
	const events: AgentEvent[] = [];
	const stream = agentLoop([userMessage(forgeCase.prompt)], context, { model: forgeEvalModel, convertToLlm: identityEvalConverter }, undefined, scripted.streamFn);
	for await (const event of stream) events.push(event);
	const messages = await stream.result();
	const toolResults = events
		.filter((event): event is Extract<AgentEvent, { type: "tool_execution_end" }> => event.type === "tool_execution_end")
		.map((event) => {
			const text = textContent(event.result);
			return { toolName: event.toolName, isError: event.isError, text, bytes: Buffer.byteLength(text, "utf8") };
		});
	const finalAssistantText = messages
		.filter((message): message is Extract<AgentMessage, { role: "assistant" }> => message.role === "assistant")
		.map((message) => message.content.filter((content) => content.type === "text").map((content) => content.text).join("\n"))
		.at(-1) ?? "";
	const trace: EvalTrace = {
		modelSteps: scripted.modelStepCount,
		modelContexts: scripted.contexts,
		serviceCalls: service.calls,
		unexpectedServiceCalls: service.unexpectedCalls,
		toolResults,
		finalAssistantText,
		completed: finalAssistantText.length > 0 && finalAssistantText !== "__FORGE_EVAL_SCRIPT_EXHAUSTED__",
		scriptExhausted: scripted.scriptExhausted,
	};
	const assertions = makeAssertions(forgeCase, trace, service);
	return { id: forgeCase.id, category: forgeCase.category, passed: assertions.every((assertion) => assertion.passed), assertions, trace };
}

export async function runForgeEvals(cases: readonly ForgeEvalCase[] = forgeEvalCases): Promise<ForgeEvalReport> {
	const results: ForgeEvalCaseResult[] = [];
	for (const forgeCase of cases) results.push(await runForgeEvalCase(forgeCase));
	const passed = results.filter((result) => result.passed).length;
	return {
		schemaVersion: 1,
		mode: "scripted",
		generatedAt: new Date().toISOString(),
		summary: { total: results.length, passed, failed: results.length - passed, protocolPassRate: results.length === 0 ? 1 : passed / results.length },
		cases: results,
	};
}

export async function writeForgeEvalReport(report: ForgeEvalReport, outputDirectory = DEFAULT_OUTPUT): Promise<void> {
	await mkdir(outputDirectory, { recursive: true });
	await writeFile(resolve(outputDirectory, "report.json"), `${JSON.stringify(report, null, 2)}\n`, "utf8");
	await writeFile(resolve(outputDirectory, "report.html"), renderForgeEvalReport(report), "utf8");
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
	const report = await runForgeEvals();
	const outputDirectory = process.argv.includes("--output")
		? resolve(process.argv[process.argv.indexOf("--output") + 1] ?? DEFAULT_OUTPUT)
		: DEFAULT_OUTPUT;
	await writeForgeEvalReport(report, outputDirectory);
	process.stdout.write(`${JSON.stringify(report.summary)}\n`);
	if (report.summary.failed > 0) process.exitCode = 1;
}
