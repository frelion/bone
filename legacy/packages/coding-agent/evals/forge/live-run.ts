import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { agentLoop, type AgentContext, type AgentEvent, type AgentMessage, type StreamFn } from "../../../agent/src/index.ts";
import type { Api, Model } from "@frelion/bone-ai";
import { getAgentDir } from "../../src/config.ts";
import { convertToLlm } from "../../src/core/messages.ts";
import { resolveCliModel } from "../../src/core/model-resolver.ts";
import { ModelRuntime } from "../../src/core/model-runtime.ts";
import { SettingsManager } from "../../src/core/settings-manager.ts";
import { createForgeToolDefinitions } from "../../src/core/forge/tools.ts";
import { wrapToolDefinition } from "../../src/core/tools/tool-definition-wrapper.ts";
import { FakeForgeService } from "./fake-forge-service.ts";
import { liveForgeEvalCases } from "./live-cases.ts";
import { renderForgeEvalReport } from "./report.ts";
import type { LiveForgeEvalCase, LiveForgeEvalCaseResult, LiveForgeEvalReport } from "./types.ts";

const DEFAULT_OUTPUT = resolve(dirname(fileURLToPath(import.meta.url)), "../../../../.artifacts/forge-eval/live");
const LIVE_SYSTEM_PROMPT = `You are evaluating Bone's Forge tools. Complete the user's repository task using only the available Forge tools. Follow each tool schema exactly. After tool results, answer briefly. Never invent a successful result.`;

export interface LiveForgeEvalOptions {
	model: Model<Api>;
	streamFn: StreamFn;
	repetitions?: number;
	maxTurns?: number;
	timeoutMs?: number;
	cases?: readonly LiveForgeEvalCase[];
}

function userMessage(content: string): AgentMessage {
	return { role: "user", content, timestamp: Date.now() };
}

function resultText(result: any): string {
	return result?.content?.filter((item: any) => item.type === "text").map((item: any) => item.text).join("\n") ?? "";
}

function redactTraceText(value: string): string {
	return value
		.slice(0, 16 * 1024)
		.replace(/(authorization|api[_-]?key|access[_-]?token|refresh[_-]?token|password|secret|github_token|gitlab_token)\s*[:=]\s*[^,\s}]+/gi, "$1=[redacted]");
}

function redactTraceValue(value: unknown, depth = 0): unknown {
	if (depth > 4) return "[nested value omitted]";
	if (typeof value === "string") return redactTraceText(value);
	if (Array.isArray(value)) return value.slice(0, 50).map((entry) => redactTraceValue(entry, depth + 1));
	if (!value || typeof value !== "object") return value;
	const output: Record<string, unknown> = {};
	for (const [key, entry] of Object.entries(value as Record<string, unknown>).slice(0, 80)) {
		if (/^(authorization|api[_-]?key|access[_-]?token|refresh[_-]?token|password|secret|github_token|gitlab_token)$/i.test(key)) output[key] = "[redacted]";
		else output[key] = redactTraceValue(entry, depth + 1);
	}
	return output;
}

function assistantText(messages: AgentMessage[]): string {
	return messages
		.filter((message): message is Extract<AgentMessage, { role: "assistant" }> => message.role === "assistant")
		.map((message) => message.content.filter((item) => item.type === "text").map((item) => item.text).join("\n"))
		.at(-1) ?? "";
}

function countDuplicateFailures(texts: readonly string[]): number {
	return texts.filter((text) => text.includes('"code":"duplicate_failed_call"') || text.includes('"code": "duplicate_failed_call"')).length;
}

export async function runLiveForgeEvalCase(
	forgeCase: LiveForgeEvalCase,
	run: number,
	options: Omit<Required<Pick<LiveForgeEvalOptions, "maxTurns" | "timeoutMs">>, never> & Pick<LiveForgeEvalOptions, "model" | "streamFn">,
): Promise<LiveForgeEvalCaseResult> {
	const service = new FakeForgeService(forgeCase.service, {
		ignoreRequestId: true,
		auxiliary: (toolName, input) => {
			if (toolName === "forge_context") {
				return {
					handled: true,
					value: {
						repository: { provider: "github", host: "github.com", owner: "example", name: "repo", remote: "origin" },
						instance: { provider: "github", host: "github.com" },
						capabilities: {},
						user: { id: 1, login: "forge-eval" },
					},
				};
			}
			if (toolName === "forge_query" && input.operation === "get" && input.resource === "issue" && forgeCase.expectedServiceTools[0] === "forge_issue") {
				const id = Number(input.id);
				return { handled: true, value: { resource: "issue", mode: "detail", item: { id, iid: id, title: `Issue ${id}`, state: "open" } } };
			}
			return undefined;
		},
	});
	const definitions = createForgeToolDefinitions({ cwd: "/forge-eval", service });
	const tools = Object.values(definitions).map((definition) => wrapToolDefinition(definition));
	const context: AgentContext = { systemPrompt: LIVE_SYSTEM_PROMPT, messages: [], tools };
	const events: AgentEvent[] = [];
	const contexts: LiveForgeEvalCaseResult["trace"]["modelContexts"] = [];
	let modelSteps = 0;
	let turnLimitReached = false;
	let timedOut = false;
	const controller = new AbortController();
	const timer = setTimeout(() => {
		timedOut = true;
		controller.abort();
	}, options.timeoutMs);
	const observedStream: StreamFn = (model, llmContext, streamOptions) => {
		modelSteps += 1;
		const bytes = Buffer.byteLength(JSON.stringify(llmContext.messages), "utf8");
		contexts.push({
			step: modelSteps,
			messageCount: llmContext.messages.length,
			bytes,
			hasToolResult: llmContext.messages.some((message) => message.role === "toolResult"),
		});
		if (modelSteps > options.maxTurns) turnLimitReached = true;
		return options.streamFn(model, llmContext, { ...streamOptions, signal: controller.signal, maxRetries: 0 });
	};

	let messages: AgentMessage[] = [];
	try {
		const stream = agentLoop(
			[userMessage(forgeCase.prompt)],
			context,
			{ model: options.model, convertToLlm, toolExecution: "sequential", shouldStopAfterTurn: () => modelSteps >= options.maxTurns },
			controller.signal,
			observedStream,
		);
		for await (const event of stream) events.push(event);
		messages = await stream.result();
	} finally {
		clearTimeout(timer);
	}

	const toolCalls = events
		.filter((event): event is Extract<AgentEvent, { type: "tool_execution_start" }> => event.type === "tool_execution_start")
		.map((event) => ({ name: event.toolName, args: redactTraceValue(event.args) }));
	const toolResults = events
		.filter((event): event is Extract<AgentEvent, { type: "tool_execution_end" }> => event.type === "tool_execution_end")
		.map((event) => {
			const text = resultText(event.result);
			const safeText = redactTraceText(text);
			return { toolName: event.toolName, isError: event.isError, text: safeText, bytes: Buffer.byteLength(safeText, "utf8") };
		});
	const finalAssistantText = assistantText(messages);
	const errorTexts = toolResults.filter((result) => result.isError).map((result) => result.text);
	const validationFailed = errorTexts.some((text) => text.includes("Validation failed for tool"));
	const serviceTools = service.calls.map((call) => call.toolName).filter((name) => forgeCase.expectedServiceTools.includes(name));
	const expectedToolsMatch = JSON.stringify(serviceTools) === JSON.stringify(forgeCase.expectedServiceTools);
	const visibleText = [...toolResults.map((result) => result.text), finalAssistantText].join("\n");
	const assertions: LiveForgeEvalCaseResult["assertions"] = [
		{ name: "expected Forge operations complete", passed: expectedToolsMatch, detail: `expected ${JSON.stringify(forgeCase.expectedServiceTools)}, got ${JSON.stringify(serviceTools)}` },
		{ name: "fake service receives no unexpected calls", passed: service.unexpectedCalls.length === 0, detail: JSON.stringify(service.unexpectedCalls) },
		{ name: "model returns a final answer", passed: finalAssistantText.length > 0 },
		{ name: "run stays within timeout", passed: !timedOut },
		{ name: "run stays within model-turn limit", passed: !turnLimitReached },
	];
	for (const sentinel of forgeCase.mustNotContain ?? []) assertions.push({ name: `visible output omits ${sentinel}`, passed: !visibleText.includes(sentinel) });
	const firstRelevantCall = toolCalls.find((call) => {
		if (call.name === "forge_context") return false;
		if (forgeCase.expectedServiceTools[0] === "forge_issue" && call.name === "forge_query") return false;
		return true;
	});
	const firstToolSelectionCorrect = firstRelevantCall?.name === forgeCase.expectedServiceTools[0];
	const firstCallSchemaValid = toolResults.length > 0 && !toolResults[0].text.includes("Validation failed for tool");
	const completed = assertions.every((assertion) => assertion.passed);
	const contextBytes = contexts.reduce((sum, item) => sum + item.bytes, 0);
	const duplicateCount = countDuplicateFailures(errorTexts);
	return {
		id: forgeCase.id,
		category: forgeCase.category,
		run,
		passed: completed,
		metrics: {
			firstToolSelectionCorrect,
			firstCallSchemaValid,
			correctionSucceeded: validationFailed ? completed : null,
			duplicateFailureCount: duplicateCount,
			toolCallCount: toolCalls.length,
			contextBytes,
		},
		assertions,
		trace: {
			run,
			model: `${options.model.provider}/${options.model.id}`,
			modelSteps,
			modelContexts: contexts,
			serviceCalls: service.calls.map((call) => ({ ...call, input: redactTraceValue(call.input) as Record<string, unknown> })),
			unexpectedServiceCalls: service.unexpectedCalls.map((call) => ({ ...call, input: redactTraceValue(call.input) as Record<string, unknown> })),
			toolCalls,
			toolResults,
			finalAssistantText,
			completed,
			scriptExhausted: false,
			timedOut,
			turnLimitReached,
		},
	};
}

function ratio(numerator: number, denominator: number): number {
	return denominator === 0 ? 1 : numerator / denominator;
}

export async function runLiveForgeEvals(options: LiveForgeEvalOptions): Promise<LiveForgeEvalReport> {
	const repetitions = options.repetitions ?? 1;
	const maxTurns = options.maxTurns ?? 6;
	const timeoutMs = options.timeoutMs ?? 120_000;
	const cases = options.cases ?? liveForgeEvalCases;
	const results: LiveForgeEvalCaseResult[] = [];
	for (let run = 1; run <= repetitions; run += 1) {
		for (const forgeCase of cases) results.push(await runLiveForgeEvalCase(forgeCase, run, { ...options, maxTurns, timeoutMs }));
	}
	const passed = results.filter((result) => result.passed).length;
	const correctionRuns = results.filter((result) => result.metrics.correctionSucceeded !== null);
	const totalToolCalls = results.reduce((sum, result) => sum + result.metrics.toolCallCount, 0);
	return {
		schemaVersion: 1,
		mode: "live",
		generatedAt: new Date().toISOString(),
		model: { provider: options.model.provider, id: options.model.id },
		configuration: { repetitions, maxTurns, timeoutMs },
		summary: {
			total: results.length,
			passed,
			failed: results.length - passed,
			taskCompletionRate: ratio(passed, results.length),
			firstToolSelectionRate: ratio(results.filter((result) => result.metrics.firstToolSelectionCorrect).length, results.length),
			firstCallValidRate: ratio(results.filter((result) => result.metrics.firstCallSchemaValid).length, results.length),
			correctionSuccessRate: correctionRuns.length === 0 ? null : ratio(correctionRuns.filter((result) => result.metrics.correctionSucceeded).length, correctionRuns.length),
			deterministicRepeatRate: totalToolCalls === 0 ? null : ratio(results.reduce((sum, result) => sum + result.metrics.duplicateFailureCount, 0), totalToolCalls),
			meanToolCalls: ratio(totalToolCalls, results.length),
			meanContextBytes: ratio(results.reduce((sum, result) => sum + result.metrics.contextBytes, 0), results.length),
		},
		cases: results,
	};
}

export async function writeLiveForgeEvalReport(report: LiveForgeEvalReport, outputDirectory = DEFAULT_OUTPUT): Promise<void> {
	await mkdir(outputDirectory, { recursive: true });
	await writeFile(resolve(outputDirectory, "report.json"), `${JSON.stringify(report, null, 2)}\n`, "utf8");
	await writeFile(resolve(outputDirectory, "report.html"), renderForgeEvalReport(report), "utf8");
}

interface LiveCliOptions { model?: string; caseId?: string; repetitions: number; maxTurns: number; timeoutMs: number; output: string }

export function parseLiveForgeEvalArgs(args: readonly string[]): LiveCliOptions {
	const parsed: LiveCliOptions = { repetitions: 1, maxTurns: 6, timeoutMs: 120_000, output: DEFAULT_OUTPUT };
	const integer = (name: string, value: string | undefined, minimum: number): number => {
		const number = Number(value);
		if (!Number.isInteger(number) || number < minimum) throw new Error(`${name} must be an integer >= ${minimum}`);
		return number;
	};
	for (let index = 0; index < args.length; index += 1) {
		const arg = args[index];
		if (arg === "--model") parsed.model = args[++index];
		else if (arg === "--case") parsed.caseId = args[++index];
		else if (arg === "--runs") parsed.repetitions = integer("--runs", args[++index], 1);
		else if (arg === "--max-turns") parsed.maxTurns = integer("--max-turns", args[++index], 1);
		else if (arg === "--timeout-ms") parsed.timeoutMs = integer("--timeout-ms", args[++index], 1_000);
		else if (arg === "--output") parsed.output = resolve(args[++index] ?? "");
		else throw new Error(`Unknown live Forge eval option: ${arg}`);
	}
	return parsed;
}

async function resolveLiveModel(modelReference: string | undefined): Promise<{ runtime: ModelRuntime; model: Model<Api> }> {
	const runtime = await ModelRuntime.create();
	if (modelReference) {
		const resolved = resolveCliModel({ cliModel: modelReference, modelRuntime: runtime });
		if (!resolved.model || resolved.error) throw new Error(resolved.error ?? `Model not found: ${modelReference}`);
		if (!runtime.hasConfiguredAuth(resolved.model.provider)) throw new Error(`No configured authentication for ${resolved.model.provider}`);
		return { runtime, model: resolved.model };
	}
	const settings = SettingsManager.create(process.cwd(), getAgentDir());
	const defaultProvider = settings.getDefaultProvider();
	const defaultModel = settings.getDefaultModel();
	if (defaultProvider && defaultModel) {
		const model = runtime.getModel(defaultProvider, defaultModel);
		if (model && runtime.hasConfiguredAuth(model.provider)) return { runtime, model };
	}
	const available = await runtime.getAvailable();
	if (!available[0]) throw new Error("No configured model is available. Configure one in /settings or pass --model provider/model.");
	return { runtime, model: available[0] };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
	try {
		const cli = parseLiveForgeEvalArgs(process.argv.slice(2));
		const { runtime, model } = await resolveLiveModel(cli.model);
		const cases = cli.caseId ? liveForgeEvalCases.filter((forgeCase) => forgeCase.id === cli.caseId) : liveForgeEvalCases;
		if (cases.length === 0) throw new Error(`Unknown live Forge eval case: ${cli.caseId}`);
		process.stdout.write(`Running live Forge eval with ${model.provider}/${model.id}; Forge remote is fake and no repository will be modified.\n`);
		const report = await runLiveForgeEvals({ model, streamFn: runtime.streamSimple.bind(runtime), repetitions: cli.repetitions, maxTurns: cli.maxTurns, timeoutMs: cli.timeoutMs, cases });
		await writeLiveForgeEvalReport(report, cli.output);
		process.stdout.write(`${JSON.stringify(report.summary)}\n`);
		if (report.summary.failed > 0) process.exitCode = 1;
	} catch (error) {
		process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
		process.exitCode = 1;
	}
}
