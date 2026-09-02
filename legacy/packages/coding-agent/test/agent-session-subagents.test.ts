import { existsSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { SubagentHandle } from "@frelion/bone-agent-core";
import {
	type Context,
	type FauxResponseFactory,
	fauxAssistantMessage,
	fauxToolCall,
	registerFauxProvider,
	streamSimple,
} from "@frelion/bone-ai/compat";
import { afterEach, describe, expect, it } from "vitest";
import { AuthStorage } from "../src/core/auth-storage.ts";
import { DefaultResourceLoader } from "../src/core/resource-loader.ts";
import { createAgentSession } from "../src/core/sdk.ts";
import { SessionManager } from "../src/core/session-manager.ts";
import { SettingsManager } from "../src/core/settings-manager.ts";
import { createInMemoryModelRegistry, getModelRuntime } from "./model-runtime-test-utils.ts";
import { stageResponse } from "./suite/harness.ts";

function lastToolResult(context: Context) {
	return context.messages
		.slice()
		.reverse()
		.find((message) => message.role === "toolResult");
}

function userText(context: Context): string {
	const message = context.messages
		.slice()
		.reverse()
		.find((candidate) => candidate.role === "user");
	if (!message || message.role !== "user") return "";
	return typeof message.content === "string"
		? message.content
		: message.content.flatMap((part) => (part.type === "text" ? [part.text] : [])).join("");
}

describe("AgentSession subagent integration", () => {
	const cleanups: Array<() => void> = [];

	afterEach(() => {
		for (const cleanup of cleanups.splice(0)) cleanup();
	});

	it("delegates to an isolated read-only child, waits, and retains it for follow-up", async () => {
		const cwd = join(tmpdir(), `bone-subagent-integration-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		const agentDir = join(cwd, ".agent");
		mkdirSync(agentDir, { recursive: true });

		const faux = registerFauxProvider();
		const model = faux.getModel();
		const authStorage = AuthStorage.inMemory();
		await authStorage.modify(model.provider, async () => ({ type: "api_key", key: "faux-key" }));
		const registry = await createInMemoryModelRegistry(authStorage);
		registry.registerProvider(model.provider, {
			baseUrl: model.baseUrl,
			apiKey: "faux-key",
			api: faux.api,
			streamSimple,
			models: faux.models,
		});
		const modelRuntime = getModelRuntime(registry);
		const settingsManager = SettingsManager.inMemory();
		const resourceLoader = new DefaultResourceLoader({ cwd, agentDir, settingsManager });
		await resourceLoader.reload();

		const childToolSnapshots: string[][] = [];
		const respond: FauxResponseFactory = (context) => {
			const toolNames = context.tools.map((tool) => tool.name);
			const isParent = toolNames.includes("delegate_stage");
			const lastResult = lastToolResult(context);

			if (!isParent) {
				childToolSnapshots.push(toolNames);
				if (userText(context).includes("Why is that the conclusion?")) {
					return fauxAssistantMessage("Follow-up answer from retained child context.");
				}
				if (!lastResult) return stageResponse("CHILD_PRIVATE_NOISE", "Inspect delegated scope");
				return fauxAssistantMessage("Child summary with bounded evidence.");
			}

			if (!lastResult) return stageResponse("Delegating the investigation.", "Delegate investigation");
			if (lastResult.toolName === "set_action") {
				return fauxAssistantMessage(
					[
						fauxToolCall("delegate_stage", {
							objective: "Investigate the isolated subsystem",
							label: "Subsystem investigator",
							scope: "conversation",
							contextRefs: ["src/example.ts"],
							expectedOutput: "A concise evidence-based conclusion",
						}),
					],
					{ stopReason: "toolUse" },
				);
			}
			if (lastResult.toolName === "delegate_stage") {
				const handle = lastResult.details as SubagentHandle;
				return fauxAssistantMessage([fauxToolCall("wait_agent", { agentRef: handle.id })], {
					stopReason: "toolUse",
				});
			}
			return fauxAssistantMessage("Parent final answer based on the bounded child handoff.");
		};
		faux.setResponses(Array.from({ length: 12 }, () => respond));

		const { session } = await createAgentSession({
			cwd,
			agentDir,
			model,
			modelRuntime,
			settingsManager,
			resourceLoader,
			sessionManager: SessionManager.inMemory(cwd),
		});
		cleanups.push(() => {
			session.dispose();
			faux.unregister();
			if (existsSync(cwd)) rmSync(cwd, { recursive: true, force: true });
		});

		expect(session.getActiveToolNames()).toEqual(
			expect.arrayContaining(["delegate_stage", "ask_agent", "wait_agent", "cancel_agent", "close_agent"]),
		);
		expect(session.systemPrompt).toContain("- delegate_stage:");

		await session.prompt("Use a child agent for this investigation.");

		const execution = session.subagentProjection.executions[0];
		expect(execution).toMatchObject({
			label: "Subsystem investigator",
			scope: "conversation",
			status: "idle",
			lastRunStatus: "completed",
			handoff: { status: "completed", summary: "Child summary with bounded evidence." },
		});
		expect(session.getLastAssistantText()).toBe("Parent final answer based on the bounded child handoff.");
		expect(JSON.stringify(session.messages)).not.toContain("CHILD_PRIVATE_NOISE");
		expect(childToolSnapshots.length).toBeGreaterThan(0);
		for (const tools of childToolSnapshots) {
			expect(tools).toEqual(expect.arrayContaining(["read", "grep", "find", "ls", "yield_to_parent"]));
			expect(tools).not.toEqual(expect.arrayContaining(["bash", "edit", "write", "delegate_stage"]));
		}

		const followUp = await session.subagentManager!.runtime.ask(execution!.agentRef, "Why is that the conclusion?");
		expect(followUp.summary).toBe("Follow-up answer from retained child context.");

		await session.shutdownSubagents();
		expect(session.subagentManager!.runtime.get(execution!.agentRef)?.status).toBe("closed");
	});

	it("disables child-agent creation recursively when subagents is false", async () => {
		const cwd = join(tmpdir(), `bone-subagent-disabled-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		const agentDir = join(cwd, ".agent");
		mkdirSync(agentDir, { recursive: true });
		const faux = registerFauxProvider();
		const model = faux.getModel();
		const authStorage = AuthStorage.inMemory();
		await authStorage.modify(model.provider, async () => ({ type: "api_key", key: "faux-key" }));
		const registry = await createInMemoryModelRegistry(authStorage);
		registry.registerProvider(model.provider, {
			baseUrl: model.baseUrl,
			apiKey: "faux-key",
			api: faux.api,
			streamSimple,
			models: faux.models,
		});

		const { session } = await createAgentSession({
			cwd,
			agentDir,
			model,
			modelRuntime: getModelRuntime(registry),
			settingsManager: SettingsManager.inMemory(),
			sessionManager: SessionManager.inMemory(cwd),
			subagents: false,
		});
		cleanups.push(() => {
			session.dispose();
			faux.unregister();
			if (existsSync(cwd)) rmSync(cwd, { recursive: true, force: true });
		});

		expect(session.subagentManager).toBeUndefined();
		expect(session.getAllTools().map((tool) => tool.name)).not.toContain("delegate_stage");
	});

	it("closes an exchange-scoped child when a queued follow-up starts the next Exchange", async () => {
		const cwd = join(tmpdir(), `bone-subagent-follow-up-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		const agentDir = join(cwd, ".agent");
		mkdirSync(agentDir, { recursive: true });

		const faux = registerFauxProvider();
		const model = faux.getModel();
		const authStorage = AuthStorage.inMemory();
		await authStorage.modify(model.provider, async () => ({ type: "api_key", key: "faux-key" }));
		const registry = await createInMemoryModelRegistry(authStorage);
		registry.registerProvider(model.provider, {
			baseUrl: model.baseUrl,
			apiKey: "faux-key",
			api: faux.api,
			streamSimple,
			models: faux.models,
		});
		const modelRuntime = getModelRuntime(registry);
		const settingsManager = SettingsManager.inMemory();
		const resourceLoader = new DefaultResourceLoader({ cwd, agentDir, settingsManager });
		await resourceLoader.reload();

		let releaseChild!: () => void;
		const childGate = new Promise<void>((resolve) => {
			releaseChild = resolve;
		});
		let markChildStarted!: () => void;
		const childStarted = new Promise<void>((resolve) => {
			markChildStarted = resolve;
		});
		const respond: FauxResponseFactory = async (context) => {
			const isParent = context.tools.some((tool) => tool.name === "delegate_stage");
			const latest = context.messages.at(-1);
			if (!isParent) {
				markChildStarted();
				await childGate;
				return fauxAssistantMessage("Exchange-scoped child handoff.");
			}
			if (latest?.role === "user") {
				return userText(context).includes("queued follow-up")
					? fauxAssistantMessage("Queued follow-up complete.")
					: stageResponse("Delegating before the queued follow-up.", "Delegate exchange-scoped work");
			}
			if (latest?.role === "toolResult" && latest.toolName === "set_action") {
				return fauxAssistantMessage(
					[
						fauxToolCall("delegate_stage", {
							objective: "Complete work owned only by the first Exchange",
							label: "First Exchange child",
							scope: "exchange",
						}),
					],
					{ stopReason: "toolUse" },
				);
			}
			if (latest?.role === "toolResult" && latest.toolName === "delegate_stage") {
				const handle = latest.details as SubagentHandle;
				return fauxAssistantMessage([fauxToolCall("wait_agent", { agentRef: handle.id })], {
					stopReason: "toolUse",
				});
			}
			return fauxAssistantMessage("First Exchange complete.");
		};
		faux.setResponses(Array.from({ length: 12 }, () => respond));

		const { session } = await createAgentSession({
			cwd,
			agentDir,
			model,
			modelRuntime,
			settingsManager,
			resourceLoader,
			sessionManager: SessionManager.inMemory(cwd),
		});
		cleanups.push(() => {
			releaseChild();
			session.dispose();
			faux.unregister();
			if (existsSync(cwd)) rmSync(cwd, { recursive: true, force: true });
		});

		const prompt = session.prompt("Start the first Exchange.");
		await childStarted;
		await session.followUp("Handle this queued follow-up in the next Exchange.");
		releaseChild();
		await prompt;

		expect(session.exchangeProjection.exchanges).toHaveLength(2);
		expect(session.exchangeProjection.exchanges.map((exchange) => exchange.status)).toEqual([
			"completed",
			"completed",
		]);
		expect(session.exchangeProjection.exchanges[1]?.inputs).toMatchObject([{ delivery: "follow_up" }]);
		const execution = session.subagentProjection.executions[0];
		expect(execution?.origin.exchangeId).toBe(session.exchangeProjection.exchanges[0]?.id);
		expect(execution).toMatchObject({
			scope: "exchange",
			status: "closed",
			lastRunStatus: "completed",
		});
		expect(session.subagentManager!.runtime.get(execution!.agentRef)?.status).toBe("closed");
	});

	it("keeps child yields private until read_agent_messages consumes them in order", async () => {
		const cwd = join(tmpdir(), `bone-subagent-yield-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		const agentDir = join(cwd, ".agent");
		mkdirSync(agentDir, { recursive: true });

		const faux = registerFauxProvider();
		const model = faux.getModel();
		const authStorage = AuthStorage.inMemory();
		await authStorage.modify(model.provider, async () => ({ type: "api_key", key: "faux-key" }));
		const registry = await createInMemoryModelRegistry(authStorage);
		registry.registerProvider(model.provider, {
			baseUrl: model.baseUrl,
			apiKey: "faux-key",
			api: faux.api,
			streamSimple,
			models: faux.models,
		});
		const modelRuntime = getModelRuntime(registry);
		const settingsManager = SettingsManager.inMemory();
		const resourceLoader = new DefaultResourceLoader({ cwd, agentDir, settingsManager });
		await resourceLoader.reload();

		let parentContextBeforeRead = "";
		const respond: FauxResponseFactory = (context) => {
			const toolNames = context.tools.map((tool) => tool.name);
			const isParent = toolNames.includes("delegate_stage");
			const latest = context.messages.at(-1);
			if (!isParent) {
				const yieldCount = context.messages.filter(
					(message) => message.role === "toolResult" && message.toolName === "yield_to_parent",
				).length;
				if (latest?.role === "user") return stageResponse("CHILD_YIELD_PRIVATE_WORK", "Investigate and yield");
				if (latest?.role === "toolResult" && latest.toolName === "set_action") {
					return fauxAssistantMessage(
						[
							fauxToolCall("yield_to_parent", {
								kind: "finding",
								message: "The first ordered finding.",
								artifactRefs: ["artifact://finding-1"],
							}),
						],
						{ stopReason: "toolUse" },
					);
				}
				if (yieldCount === 1) {
					return fauxAssistantMessage(
						[
							fauxToolCall("yield_to_parent", {
								kind: "risk",
								message: "The second ordered risk.",
							}),
						],
						{ stopReason: "toolUse" },
					);
				}
				return fauxAssistantMessage("Child continued after yielding and produced its final handoff.");
			}

			if (latest?.role === "user")
				return stageResponse("Delegate and consume explicit yields.", "Read child yields");
			if (latest?.role === "toolResult" && latest.toolName === "set_action") {
				return fauxAssistantMessage(
					[
						fauxToolCall("delegate_stage", {
							objective: "Yield ordered findings, continue working, then return a handoff",
							label: "Yielding child",
							scope: "conversation",
						}),
					],
					{ stopReason: "toolUse" },
				);
			}
			if (latest?.role === "toolResult" && latest.toolName === "delegate_stage") {
				const handle = latest.details as SubagentHandle;
				return fauxAssistantMessage([fauxToolCall("wait_agent", { agentRef: handle.id })], {
					stopReason: "toolUse",
				});
			}
			if (latest?.role === "toolResult" && latest.toolName === "wait_agent") {
				parentContextBeforeRead = JSON.stringify(context.messages);
				const handle = context.messages
					.filter((message) => message.role === "toolResult" && message.toolName === "delegate_stage")
					.at(-1)?.details as SubagentHandle;
				return fauxAssistantMessage([fauxToolCall("read_agent_messages", { agentRef: handle.id })], {
					stopReason: "toolUse",
				});
			}
			return fauxAssistantMessage("Parent consumed the ordered child messages.");
		};
		faux.setResponses(Array.from({ length: 16 }, () => respond));

		const { session } = await createAgentSession({
			cwd,
			agentDir,
			model,
			modelRuntime,
			settingsManager,
			resourceLoader,
			sessionManager: SessionManager.inMemory(cwd),
		});
		cleanups.push(() => {
			session.dispose();
			faux.unregister();
			if (existsSync(cwd)) rmSync(cwd, { recursive: true, force: true });
		});

		expect(session.getActiveToolNames()).toContain("read_agent_messages");
		expect(session.getActiveToolNames()).not.toContain("yield_to_parent");
		await session.prompt("Use explicit child yields.");

		expect(parentContextBeforeRead).not.toContain("The first ordered finding.");
		expect(parentContextBeforeRead).not.toContain("The second ordered risk.");
		const readResult = session.messages.find(
			(message) => message.role === "toolResult" && message.toolName === "read_agent_messages",
		);
		expect(readResult?.details).toMatchObject([
			{
				kind: "finding",
				message: "The first ordered finding.",
				artifactRefs: ["artifact://finding-1"],
				sequence: 1,
			},
			{
				kind: "risk",
				message: "The second ordered risk.",
				sequence: 2,
			},
		]);
		const execution = session.subagentProjection.executions[0];
		expect(execution?.handoff?.summary).toBe("Child continued after yielding and produced its final handoff.");
		expect(execution?.yields.map((yielded) => yielded.sequence)).toEqual([1, 2]);
		expect(execution?.unreadYieldCount).toBe(0);
		expect(session.subagentManager!.runtime.drainYields(execution!.agentRef)).toEqual([]);
	});
});
