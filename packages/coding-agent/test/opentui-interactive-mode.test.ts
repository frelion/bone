import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AgentMessage } from "@frelion/bone-agent-core";
import type { AssistantMessage } from "@frelion/bone-ai/compat";
import { TextRenderable } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type {
	AgentSession,
	AgentSessionEvent,
	AgentSessionEventListener,
	PromptOptions,
} from "../src/core/agent-session.ts";
import type { AgentSessionRuntime } from "../src/core/agent-session-runtime.ts";
import { getLastActiveConversation } from "../src/core/conversation-state.ts";
import type {
	InteractiveSessionHostHooks,
	InteractiveSessionSummary,
	RuntimeEventEnvelope,
	RuntimeStreamSnapshot,
} from "../src/core/interactive-session-host.ts";
import type { MemorySearchResult } from "../src/core/memory.ts";
import type { PlanState } from "../src/core/plan-mode.ts";
import type { QuestionAnswer, QuestionState } from "../src/core/question.ts";
import type { SessionEntry } from "../src/core/session-manager.ts";
import { OpenTUITranscriptFactory } from "../src/modes/interactive/components/opentui-transcript-factory.ts";
import {
	getOpenTUITranscriptPageStart,
	OpenTUIInteractiveMode,
	type OpenTUISessionHostContract,
} from "../src/modes/interactive/opentui-interactive-mode.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

type NativeTestRenderer = TestRendererSetup["renderer"] & {
	readonly input: TestRendererSetup["mockInput"];
	readonly mouse: TestRendererSetup["mockMouse"];
	captureFrame(): string;
	flush(): Promise<void>;
	waitForFrameText(text: string): Promise<string>;
};

const testSetups = new Set<TestRendererSetup>();

async function createNativeTestRenderer(options: { width: number; height: number }): Promise<NativeTestRenderer> {
	const setup = await createTestRenderer({ ...options, autoFocus: false, useMouse: true, exitOnCtrlC: false });
	setup.renderer.start();
	testSetups.add(setup);
	return Object.assign(setup.renderer, {
		input: setup.mockInput,
		mouse: setup.mockMouse,
		captureFrame: setup.captureCharFrame,
		flush: () => setup.flush(),
		waitForFrameText: (text: string) => setup.waitForFrame((frame) => frame.includes(text), { maxPasses: 50 }),
	}) as NativeTestRenderer;
}

afterEach(() => {
	for (const setup of testSetups) setup.renderer.destroy();
	testSetups.clear();
});

function userMessage(text: string, timestamp = 1): AgentMessage {
	return { role: "user", content: text, timestamp };
}

function assistantMessage(text: string, stopReason?: AssistantMessage["stopReason"]): AssistantMessage {
	return {
		role: "assistant",
		content: [{ type: "text", text }],
		api: "anthropic-messages",
		provider: "anthropic",
		model: "test",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason,
		timestamp: 2,
	};
}

function entry(message: AgentMessage, id: string): SessionEntry {
	return { type: "message", id, parentId: null, timestamp: new Date(0).toISOString(), message };
}

interface FakeSession {
	isStreaming: boolean;
	isCompacting: boolean;
	isBashRunning: boolean;
	planState: PlanState;
	questionState: QuestionState;
	sessionFile: string | undefined;
	sessionManager: {
		getEntries(): SessionEntry[];
		getCwd(): string;
		getSessionDir(): string;
		getSessionId(): string;
		getSessionName(): string | undefined;
	};
	subscribe(listener: AgentSessionEventListener): () => void;
	abort(): Promise<void>;
	waitForCompaction(): Promise<void>;
	executeBash(
		command: string,
		onChunk?: (chunk: string) => void,
		options?: { excludeFromContext?: boolean },
	): Promise<{ output: string; exitCode: number; cancelled: boolean; truncated: boolean }>;
	bindExtensions(): Promise<void>;
	parkExtensionUI(): void;
	approvePlan(proposalId: string): Promise<void>;
	revisePlan(proposalId: string, feedback: string): Promise<void>;
	cancelPlan(proposalId: string): void;
	answerQuestion(requestId: string, answers: QuestionAnswer[], overallNotes?: string): void;
	cancelQuestion(requestId: string, reason: "user" | "abort" | "client_disconnect" | "no_ui"): void;
	getFollowUpMessages(): readonly string[];
	getSteeringMessages(): readonly string[];
}

function createRuntime(
	entries: SessionEntry[] = [],
	metadata: { sessionFile?: string; agentDir?: string; id?: string; name?: string } = {},
) {
	const listeners = new Set<AgentSessionEventListener>();
	const abort = vi.fn(async () => {});
	const session: FakeSession = {
		isStreaming: false,
		isCompacting: false,
		isBashRunning: false,
		planState: { status: "inactive" },
		questionState: { status: "inactive" },
		sessionFile: metadata.sessionFile,
		sessionManager: {
			getEntries: () => entries,
			getCwd: () => process.cwd(),
			getSessionDir: () => "/tmp/bone-opentui-mode-sessions",
			getSessionId: () => metadata.id ?? "session-id",
			getSessionName: () => metadata.name,
		},
		subscribe: (listener) => {
			listeners.add(listener);
			return () => listeners.delete(listener);
		},
		abort,
		waitForCompaction: async () => {},
		executeBash: async () => ({ output: "", exitCode: 0, cancelled: false, truncated: false }),
		bindExtensions: async () => {},
		parkExtensionUI: vi.fn(),
		approvePlan: vi.fn(async () => {
			session.planState = { status: "inactive" };
		}),
		revisePlan: vi.fn(async () => {
			session.planState = { status: "planning" };
		}),
		cancelPlan: vi.fn(() => {
			session.planState = { status: "inactive" };
		}),
		answerQuestion: vi.fn(() => {
			session.questionState = { status: "inactive" };
		}),
		cancelQuestion: vi.fn(() => {
			session.questionState = { status: "inactive" };
		}),
		getFollowUpMessages: () => [],
		getSteeringMessages: () => [],
	};
	const runtime = {
		session,
		cwd: process.cwd(),
		services: {
			agentDir: metadata.agentDir ?? "/tmp/bone-opentui-mode-agent",
			settingsManager: {
				getShowImages: () => true,
				getHideThinkingBlock: () => false,
				getSidebarWidth: () => undefined,
				setSidebarWidth: vi.fn(),
			},
		},
	} as unknown as AgentSessionRuntime;
	return {
		runtime,
		session,
		abort,
		emit: (event: AgentSessionEvent) => {
			if (event.type === "plan_proposed")
				session.planState = { status: "awaitingApproval", proposal: event.proposal };
			if (event.type === "plan_decided") session.planState = { status: "inactive" };
			if (event.type === "question_asked")
				session.questionState = { status: "awaitingAnswer", request: event.request };
			if (event.type === "question_answered" || event.type === "question_cancelled")
				session.questionState = { status: "inactive" };
			for (const listener of listeners) listener(event);
		},
		listenerCount: () => listeners.size,
	};
}

class FakeMemoryRuntime {
	readonly start = vi.fn(async (_sessions: readonly InteractiveSessionSummary[]) => {});
	readonly recordPersistedEntries = vi.fn(
		async (_session: { path: string; id: string; name?: string }, _entries: readonly SessionEntry[]) => {},
	);
	readonly recordCompletedRun = vi.fn(
		async (_session: { path: string; id: string; name?: string }, _messages: readonly AgentMessage[]) => {},
	);
	readonly removeSession = vi.fn(async (_sessionPath: string) => {});
	readonly search = vi.fn(
		async (_query: string, _sessions: readonly InteractiveSessionSummary[]): Promise<MemorySearchResult[]> => [],
	);
	readonly searchSemantic = vi.fn(
		async (_query: string, _sessions: readonly InteractiveSessionSummary[]): Promise<MemorySearchResult[]> => [],
	);
	readonly dispose = vi.fn(async () => {});
}

class FakeHost implements OpenTUISessionHostContract {
	current: AgentSessionRuntime;
	hooks: InteractiveSessionHostHooks = {};
	readonly prompts: Array<{ runtime: AgentSessionRuntime; text: string; options?: PromptOptions }> = [];
	readonly activate = vi.fn(async () => {});
	readonly createNew = vi.fn(async () => {});
	readonly deleteSession = vi.fn(async () => ({ method: "discarded" as const }));
	readonly disposeAll = vi.fn(async () => {});
	readonly listPage = vi.fn(async () => ({
		sessions: [] as InteractiveSessionSummary[],
		total: 0,
		hasMore: false,
		nextOffset: 0,
	}));
	readonly getSessionState = vi.fn((_sessionPath: string) => "cold" as const);
	readonly getSessionPresentation = vi.fn((_sessionPath: string) => ({ state: "cold" as const }));
	readonly getSessionSummaries = vi.fn(async (_paths: readonly string[]) => [] as InteractiveSessionSummary[]);
	private readonly streamState = new WeakMap<
		AgentSessionRuntime,
		{
			revision: number;
			generationId: string;
			liveEvents: AgentSessionEvent[];
			liveEventRevisions: number[];
			liveEventGenerationIds: string[];
		}
	>();

	constructor(runtime: AgentSessionRuntime) {
		this.current = runtime;
	}

	setHooks(hooks: InteractiveSessionHostHooks): void {
		this.hooks = hooks;
	}

	async prompt(runtime: AgentSessionRuntime, text: string, options?: PromptOptions): Promise<void> {
		this.prompts.push({ runtime, text, options });
	}

	seedRuntimeStream(
		runtime: AgentSessionRuntime,
		generationId: string,
		envelopes: Array<{ revision: number; generationId: string; event: AgentSessionEvent }>,
	): void {
		this.streamState.set(runtime, {
			revision: Math.max(0, ...envelopes.map((envelope) => envelope.revision)),
			generationId,
			liveEvents: envelopes.map((envelope) => envelope.event),
			liveEventRevisions: envelopes.map((envelope) => envelope.revision),
			liveEventGenerationIds: envelopes.map((envelope) => envelope.generationId),
		});
	}

	getRuntimeStreamSnapshot(runtime: AgentSessionRuntime): RuntimeStreamSnapshot {
		const state = this.streamState.get(runtime) ?? {
			revision: 0,
			generationId: "generation-0",
			liveEvents: [],
			liveEventRevisions: [],
			liveEventGenerationIds: [],
		};
		return {
			revision: state.revision,
			generationId: state.generationId,
			liveEvents: state.liveEvents.slice(),
			liveEventEnvelopes: state.liveEvents.map((event, index) => ({
				runtime,
				revision: state.liveEventRevisions[index] ?? state.revision,
				generationId: state.liveEventGenerationIds[index] ?? state.generationId,
				event,
			})),
		};
	}

	subscribeRuntime(runtime: AgentSessionRuntime, listener: (envelope: RuntimeEventEnvelope) => void): () => void {
		const state = this.streamState.get(runtime) ?? {
			revision: 0,
			generationId: "generation-0",
			liveEvents: [],
			liveEventRevisions: [],
			liveEventGenerationIds: [],
		};
		this.streamState.set(runtime, state);
		return runtime.session.subscribe((event) => {
			state.revision++;
			if (event.type === "agent_start") {
				state.generationId = `generation-${state.revision}`;
				state.liveEvents = [];
				state.liveEventRevisions = [];
				state.liveEventGenerationIds = [];
			}
			state.liveEvents.push(event);
			state.liveEventRevisions.push(state.revision);
			state.liveEventGenerationIds.push(state.generationId);
			listener({ runtime, revision: state.revision, generationId: state.generationId, event });
			if (event.type === "agent_settled") {
				state.liveEvents = [];
				state.liveEventRevisions = [];
				state.liveEventGenerationIds = [];
			}
		});
	}
}

async function settle(renderer: NativeTestRenderer): Promise<void> {
	for (let index = 0; index < 8; index++) await Promise.resolve();
	await renderer.flush();
}

function sessionSummary(path: string, overrides: Partial<InteractiveSessionSummary> = {}): InteractiveSessionSummary {
	return {
		path,
		id: path,
		cwd: process.cwd(),
		created: new Date(0),
		modified: new Date(0),
		messageCount: 1,
		firstMessage: `Conversation ${path}`,
		allMessagesText: `Conversation ${path}`,
		state: "cold",
		...overrides,
	};
}

beforeEach(() => initTheme("dark"));

describe("OpenTUIInteractiveMode", () => {
	test("renders only the latest complete-turn page for large histories", async () => {
		const entries = Array.from({ length: 600 }, (_, index) =>
			entry(
				index % 2 === 0
					? userMessage(`prompt ${index / 2}`)
					: assistantMessage(`answer ${(index - 1) / 2}`, "stop"),
				`entry-${index}`,
			),
		);
		const firstPageStart = getOpenTUITranscriptPageStart(entries);
		expect(firstPageStart).toBe(500);
		expect(entries[firstPageStart]).toMatchObject({ type: "message", message: { role: "user" } });
		expect(getOpenTUITranscriptPageStart(entries, firstPageStart)).toBe(400);

		const active = createRuntime(entries);
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory(renderer);
		const createSessionEntries = vi.spyOn(transcriptFactory, "createSessionEntries");
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			createTranscriptFactory: () => transcriptFactory,
			installSignalHandlers: false,
		});

		await mode.init();
		await settle(renderer);
		expect(createSessionEntries).toHaveBeenNthCalledWith(1, entries.slice(500));
		expect(renderer.captureFrame()).toContain("prompt 299");
		mode.stop();
	});

	test("renders initial history and routes the fixed composer submit action", async () => {
		const session = createRuntime([
			entry(userMessage("earlier prompt"), "one"),
			entry(assistantMessage("earlier answer", "stop"), "two"),
		]);
		const host = new FakeHost(session.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory(renderer);
		vi.spyOn(transcriptFactory, "createSessionEntry").mockImplementation(async (sessionEntry) => ({
			key: sessionEntry.id,
			root: new TextRenderable(renderer, {
				content: sessionEntry.type === "message" ? JSON.stringify(sessionEntry.message.content) : sessionEntry.type,
			}),
		}));
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			createTranscriptFactory: () => transcriptFactory,
			installSignalHandlers: false,
		});

		await mode.init();
		await settle(renderer);
		expect(renderer.captureFrame()).toContain("earlier prompt");
		expect(renderer.captureFrame()).toContain("earlier answer");

		await renderer.input.typeText("next prompt");
		renderer.input.pressEnter();
		await mode.idle();
		await settle(renderer);
		expect(host.prompts).toEqual([
			expect.objectContaining({ runtime: session.runtime, text: "next prompt", options: { source: "interactive" } }),
		]);
		mode.stop();
		await vi.waitFor(() => expect(host.disposeAll).toHaveBeenCalledOnce());
	});

	test("keeps the composer focused when clicking the transcript", async () => {
		const session = createRuntime([entry(userMessage("earlier prompt"), "one")]);
		const host = new FakeHost(session.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});

		await mode.init();
		await settle(renderer);
		await renderer.mouse.click(60, 6);
		await renderer.input.typeText("draft after transcript click");
		await settle(renderer);

		expect(renderer.captureFrame()).toContain("draft after transcript click");
		mode.stop();
	});

	test("updates streaming assistant and tool output without rebuilding the shell", async () => {
		const session = createRuntime();
		const host = new FakeHost(session.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 28 });
		const transcriptFactory = new OpenTUITranscriptFactory(renderer);
		const handleEvent = vi.spyOn(transcriptFactory, "handleEvent");
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			createTranscriptFactory: () => transcriptFactory,
			installSignalHandlers: false,
		});
		await mode.init();

		session.emit({ type: "message_start", message: userMessage("current prompt") });
		session.emit({ type: "message_start", message: assistantMessage("partial") });
		session.emit({
			type: "message_update",
			message: assistantMessage("completed", "stop"),
			assistantMessageEvent: { type: "done", reason: "stop", message: assistantMessage("completed", "stop") },
		});
		await mode.idle();
		await settle(renderer);
		expect(handleEvent).toHaveBeenCalledWith(expect.objectContaining({ type: "message_update" }));
		session.emit({
			type: "tool_execution_start",
			toolCallId: "call-1",
			toolName: "read",
			args: { path: "README.md" },
		});
		session.emit({
			type: "tool_execution_update",
			toolCallId: "call-1",
			toolName: "read",
			args: { path: "README.md" },
			partialResult: { content: [{ type: "text", text: "first chunk" }] },
		});
		session.emit({
			type: "tool_execution_end",
			toolCallId: "call-1",
			toolName: "read",
			result: {
				content: [
					{
						type: "text",
						text: `${Array.from({ length: 23 }, (_, index) => `line ${index + 1}`).join("\n")}\nfinal chunk`,
					},
				],
			},
			isError: false,
		});
		await mode.idle();
		await settle(renderer);

		let frame = renderer.captureFrame();
		expect(frame).toContain("✓ Inspected the workspace · 1s · 1 tool call");
		expect(frame).not.toContain("final chunk");
		const activityRow = frame.split("\n").findIndex((line) => line.includes("Inspected the workspace"));
		expect(activityRow).toBeGreaterThanOrEqual(0);
		await renderer.mouse.click(4, activityRow);
		await settle(renderer);
		frame = renderer.captureFrame();
		const summary = "read · README.md · 24 lines";
		const summaryRow = frame.split("\n").findIndex((line) => line.includes(summary));
		expect(summaryRow).toBeGreaterThanOrEqual(0);
		await renderer.mouse.click(5, summaryRow);
		await new Promise<void>((resolve) => setImmediate(resolve));
		await settle(renderer);
		frame = renderer.captureFrame();
		expect(frame).toContain("line 1");
		expect(frame.split("\n").findIndex((line) => line.includes(summary))).toBe(summaryRow);
		renderer.input.pressKey("o", { ctrl: true });
		await settle(renderer);
		frame = renderer.captureFrame();
		expect(frame).toContain(summary);
		expect(frame).toContain("line 1");
		mode.stop();
	});

	test("coalesces queued streaming updates without crossing terminal event boundaries", async () => {
		const session = createRuntime();
		const host = new FakeHost(session.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory(renderer);
		const handleEvent = transcriptFactory.handleEvent.bind(transcriptFactory);
		let releaseFrame!: () => void;
		const frameBlocked = new Promise<void>((resolve) => {
			releaseFrame = resolve;
		});
		const waitForStreamUpdateFrame = vi.fn(async () => await frameBlocked);
		const handledTypes: AgentSessionEvent["type"][] = [];
		vi.spyOn(transcriptFactory, "handleEvent").mockImplementation(async (event) => {
			handledTypes.push(event.type);
			return await handleEvent(event);
		});
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			createTranscriptFactory: () => transcriptFactory,
			installSignalHandlers: false,
			waitForStreamUpdateFrame,
		});
		await mode.init();

		session.emit({ type: "message_start", message: userMessage("stream a response") });
		session.emit({ type: "message_start", message: assistantMessage("") });
		const firstPartial = assistantMessage("partial 0");
		session.emit({
			type: "message_update",
			message: firstPartial,
			assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "partial 0", partial: firstPartial },
		});
		await vi.waitFor(() => expect(waitForStreamUpdateFrame).toHaveBeenCalledOnce());
		for (let index = 1; index <= 1000; index++) {
			const partial = assistantMessage(`partial ${index}`);
			session.emit({
				type: "message_update",
				message: partial,
				assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: String(index), partial },
			});
			await Promise.resolve();
		}
		const idlePromise = mode.idle();
		await Promise.resolve();
		const completed = assistantMessage("final answer", "stop");
		session.emit({ type: "message_end", message: completed });
		session.emit({ type: "agent_settled" });
		releaseFrame();
		await idlePromise;
		await settle(renderer);

		expect(handledTypes.filter((type) => type === "message_update")).toHaveLength(1);
		expect(handledTypes.lastIndexOf("message_update")).toBeLessThan(handledTypes.indexOf("message_end"));
		expect(handledTypes.indexOf("message_end")).toBeLessThan(handledTypes.indexOf("agent_settled"));
		expect(renderer.captureFrame()).toContain("final answer");
		mode.stop();
	});

	test("coalesces bursty tool updates before the tool completion boundary", async () => {
		const session = createRuntime();
		const host = new FakeHost(session.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory(renderer);
		const handleEvent = vi.spyOn(transcriptFactory, "handleEvent");
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			createTranscriptFactory: () => transcriptFactory,
			installSignalHandlers: false,
		});
		await mode.init();

		session.emit({
			type: "tool_execution_start",
			toolCallId: "call-burst",
			toolName: "read",
			args: { path: "README.md" },
		});
		for (let index = 1; index <= 1000; index++) {
			session.emit({
				type: "tool_execution_update",
				toolCallId: "call-burst",
				toolName: "read",
				args: { path: "README.md" },
				partialResult: { content: [{ type: "text", text: `chunk ${index}` }] },
			});
		}
		session.emit({
			type: "tool_execution_end",
			toolCallId: "call-burst",
			toolName: "read",
			result: { content: [{ type: "text", text: "final chunk" }] },
			isError: false,
		});
		await mode.idle();

		const handledTypes = handleEvent.mock.calls.map(([event]) => event.type);
		expect(handledTypes.filter((type) => type === "tool_execution_update")).toHaveLength(1);
		expect(handledTypes.indexOf("tool_execution_start")).toBeLessThan(handledTypes.indexOf("tool_execution_update"));
		expect(handledTypes.indexOf("tool_execution_update")).toBeLessThan(handledTypes.indexOf("tool_execution_end"));
		mode.stop();
	});

	test("replays runtime events emitted while transcript history is loading", async () => {
		const active = createRuntime([entry(userMessage("earlier prompt"), "one")]);
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory(renderer);
		const handleEvent = vi.spyOn(transcriptFactory, "handleEvent");
		let releaseBinding!: () => void;
		const bindingReady = new Promise<void>((resolve) => {
			releaseBinding = resolve;
		});
		active.session.bindExtensions = async () => await bindingReady;
		let releaseHistory!: () => void;
		const historyReady = new Promise<void>((resolve) => {
			releaseHistory = resolve;
		});
		const createSessionEntries = vi
			.spyOn(transcriptFactory, "createSessionEntries")
			.mockImplementation(async (entries) => {
				await historyReady;
				return await new OpenTUITranscriptFactory(renderer).createSessionEntries(entries);
			});
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			createTranscriptFactory: () => transcriptFactory,
			installSignalHandlers: false,
		});

		const initPromise = mode.init();
		await vi.waitFor(() => expect(active.listenerCount()).toBe(1));
		const partial = assistantMessage("partial answer");
		active.emit({ type: "agent_start" });
		active.emit({ type: "message_start", message: partial });
		active.emit({
			type: "message_update",
			message: partial,
			assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "partial answer", partial },
		});
		releaseBinding();
		await vi.waitFor(() => expect(createSessionEntries).toHaveBeenCalled());
		active.emit({ type: "agent_settled" });
		releaseHistory();
		await initPromise;
		await mode.idle();
		await settle(renderer);

		expect(handleEvent).toHaveBeenCalledWith(expect.objectContaining({ type: "message_update" }));
		mode.stop();
	});

	test("generates and refreshes an unnamed conversation title after the first user message is persisted", async () => {
		const entries: SessionEntry[] = [];
		const active = createRuntime(entries, { sessionFile: "/tmp/automatic-title.jsonl" });
		const model = { provider: "test", id: "title-model" };
		const generateTitle = vi.fn(async () => {
			expect(active.session.sessionManager.getEntries()).toHaveLength(1);
			return { kind: "title" as const, title: "Automatic title" };
		});
		const setSessionName = vi.fn();
		Object.assign(active.runtime.session as AgentSession, {
			model,
			modelRuntime: { checkAuth: vi.fn(async () => true) },
			generateTitle,
			setSessionName,
		});
		Object.assign(active.runtime.services.settingsManager, { getTaskModel: () => undefined });
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		const listCallsBeforeStart = host.listPage.mock.calls.length;
		const firstUserEntry = entry(userMessage("Fix title generation"), "first-user");
		entries.push(firstUserEntry);

		await host.hooks.persistedEntries?.(active.runtime, [firstUserEntry]);
		await mode.idle();

		expect(generateTitle).toHaveBeenCalledWith(model);
		expect(setSessionName).toHaveBeenCalledWith("Automatic title");
		expect(host.listPage.mock.calls.length).toBeGreaterThan(listCallsBeforeStart);
		mode.stop();
	});

	test("does not replace a manual name while automatic title generation is in flight", async () => {
		const entries = [entry(userMessage("Fix title generation"), "first-user")];
		const active = createRuntime(entries, { sessionFile: "/tmp/manual-title-race.jsonl" });
		const model = { provider: "test", id: "title-model" };
		let currentName: string | undefined;
		let finishTitle!: (result: { kind: "title"; title: string }) => void;
		const generateTitle = vi.fn(
			async () =>
				await new Promise<{ kind: "title"; title: string }>((resolve) => {
					finishTitle = resolve;
				}),
		);
		const setSessionName = vi.fn((name: string) => {
			currentName = name;
		});
		Object.assign(active.session.sessionManager, { getSessionName: () => currentName });
		Object.assign(active.runtime.session as AgentSession, {
			model,
			modelRuntime: { checkAuth: vi.fn(async () => true) },
			generateTitle,
			setSessionName,
		});
		Object.assign(active.runtime.services.settingsManager, { getTaskModel: () => undefined });
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();

		await host.hooks.persistedEntries?.(active.runtime, entries);
		await vi.waitFor(() => expect(generateTitle).toHaveBeenCalledOnce());
		setSessionName("Manual title");
		finishTitle({ kind: "title", title: "Automatic title" });
		await mode.idle();

		expect(setSessionName).toHaveBeenCalledTimes(1);
		expect(currentName).toBe("Manual title");
		mode.stop();
	});

	test("filters replay envelopes from stale stream generations", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory(renderer);
		const handleEvent = vi.spyOn(transcriptFactory, "handleEvent");
		const stale = assistantMessage("stale generation output");
		const current = assistantMessage("current generation output");
		host.seedRuntimeStream(active.runtime, "generation-current", [
			{ revision: 2, generationId: "generation-old", event: { type: "message_start", message: stale } },
			{ revision: 3, generationId: "generation-current", event: { type: "message_start", message: current } },
		]);
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			createTranscriptFactory: () => transcriptFactory,
			installSignalHandlers: false,
		});

		await mode.init();
		await settle(renderer);
		expect(handleEvent).toHaveBeenCalledWith(expect.objectContaining({ message: current }));
		expect(handleEvent).not.toHaveBeenCalledWith(expect.objectContaining({ message: stale }));
		mode.stop();
	});

	test("rebinds foreground subscriptions and handles fixed interrupt and shutdown keys", async () => {
		const first = createRuntime([entry(userMessage("first session"), "first")]);
		const second = createRuntime([entry(userMessage("second session"), "second")]);
		const host = new FakeHost(first.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory(renderer);
		const createSessionEntry = vi.spyOn(transcriptFactory, "createSessionEntry");
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			createTranscriptFactory: () => transcriptFactory,
			installSignalHandlers: false,
		});
		await mode.init();
		expect(first.listenerCount()).toBe(1);

		await host.hooks.beforeForegroundChange?.(first.runtime);
		host.current = second.runtime;
		await host.hooks.foregroundChanged?.(second.runtime);
		expect(first.listenerCount()).toBe(0);
		expect(second.listenerCount()).toBe(1);
		await settle(renderer);
		expect(createSessionEntry).toHaveBeenCalledWith(expect.objectContaining({ id: "second" }));

		await renderer.input.typeText("draft");
		renderer.input.pressKey("d", { ctrl: true });
		await settle(renderer);
		expect(host.disposeAll).not.toHaveBeenCalled();
		renderer.input.pressCtrlC();
		await settle(renderer);

		second.session.isStreaming = true;
		renderer.input.pressCtrlC();
		await settle(renderer);
		expect(second.abort).toHaveBeenCalledOnce();

		renderer.input.pressKey("d", { ctrl: true });
		await vi.waitFor(() => expect(host.disposeAll).toHaveBeenCalledOnce());
	});

	test("restores runtime view state and forwards persistence lifecycle hooks to memory", async () => {
		const agentDir = mkdtempSync(join(tmpdir(), "bone-opentui-lifecycle-"));
		try {
			const firstPath = join(agentDir, "first.jsonl");
			const secondPath = join(agentDir, "second.jsonl");
			const first = createRuntime([], { sessionFile: firstPath, agentDir, id: "first", name: "First" });
			const second = createRuntime([], { sessionFile: secondPath, agentDir, id: "second", name: "Second" });
			const host = new FakeHost(first.runtime);
			const memory = new FakeMemoryRuntime();
			const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
			const mode = new OpenTUIInteractiveMode(host, {
				createRenderer: async () => renderer,
				createMemoryRuntime: () => memory,
				installSignalHandlers: false,
			});
			await mode.init();
			await renderer.input.typeText("first draft");

			const persistedEntry = entry(userMessage("persist me"), "persisted");
			await host.hooks.persistedEntries?.(first.runtime, [persistedEntry]);
			await host.hooks.runCompleted?.(first.runtime, [assistantMessage("done", "stop")]);
			expect(memory.recordPersistedEntries).toHaveBeenCalledWith({ path: firstPath, id: "first", name: "First" }, [
				persistedEntry,
			]);
			expect(memory.recordCompletedRun).toHaveBeenCalledWith(
				{ path: firstPath, id: "first", name: "First" },
				expect.any(Array),
			);

			await host.hooks.beforeForegroundChange?.(first.runtime);
			host.current = second.runtime;
			await host.hooks.foregroundChanged?.(second.runtime);
			await renderer.input.typeText("second draft");
			await host.hooks.beforeForegroundChange?.(second.runtime);
			host.current = first.runtime;
			await host.hooks.foregroundChanged?.(first.runtime);
			renderer.input.pressEnter();
			await mode.idle();
			expect(host.prompts).toEqual([expect.objectContaining({ runtime: first.runtime, text: "first draft" })]);
			expect(getLastActiveConversation(process.cwd(), "/tmp/bone-opentui-mode-sessions", agentDir)).toBe(firstPath);
			mode.stop();
			await vi.waitFor(() => expect(memory.dispose).toHaveBeenCalledOnce());
		} finally {
			rmSync(agentDir, { recursive: true, force: true });
		}
	});

	test("wires sidebar search results and deletion through memory", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		const visible = sessionSummary("/tmp/visible.jsonl", { state: "foreground", firstMessage: "Visible" });
		const indexed = sessionSummary("/tmp/indexed.jsonl", { firstMessage: "Indexed result" });
		host.listPage.mockResolvedValue({ sessions: [visible], total: 2, hasMore: true, nextOffset: 1 });
		host.getSessionSummaries.mockResolvedValue([indexed]);
		const memory = new FakeMemoryRuntime();
		memory.search.mockResolvedValue([
			{
				sessionPath: indexed.path,
				score: 1,
				evidence: { kind: "exchange", label: "Bone", snippet: "indexed answer" },
			},
		]);
		memory.searchSemantic.mockResolvedValue([]);
		const renderer = await createNativeTestRenderer({ width: 100, height: 28 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => memory,
			autocompleteProvider: { getSuggestions: async () => null },
			installSignalHandlers: false,
		});
		await mode.init();
		await renderer.input.typeText("/conversations");
		renderer.input.pressEnter();
		await mode.idle();
		await settle(renderer);
		renderer.input.pressKey("/");
		await settle(renderer);
		expect(renderer.captureFrame()).toContain("Search conversations");
		await renderer.input.typeText("indexed");
		await new Promise((resolvePromise) => setTimeout(resolvePromise, 120));
		await mode.idle();
		await settle(renderer);
		expect(memory.search).toHaveBeenCalledWith("indexed", expect.arrayContaining([visible]));
		expect(host.getSessionSummaries).toHaveBeenCalledWith([indexed.path]);
		expect(renderer.captureFrame()).toContain("Indexed result");

		renderer.input.pressEnter();
		await settle(renderer);
		expect(renderer.captureFrame()).toContain("Indexed result");
		renderer.input.pressKey("d");
		await settle(renderer);
		expect(renderer.captureFrame()).toContain("Press d again to delete");
		renderer.input.pressKey("d");
		await settle(renderer);
		expect(host.deleteSession).toHaveBeenCalledWith(indexed.path, visible.path);
		expect(memory.removeSession).toHaveBeenCalledWith(indexed.path);
		mode.stop();
	});

	test("keeps built-in commands out of prompts and steers ordinary input while streaming", async () => {
		const session = createRuntime();
		const host = new FakeHost(session.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();

		await renderer.input.typeText("/conversations");
		renderer.input.pressEnter();
		await mode.idle();
		await settle(renderer);
		expect(host.prompts).toHaveLength(0);
		mode.stop();

		const streamingSession = createRuntime();
		streamingSession.session.isStreaming = true;
		const streamingHost = new FakeHost(streamingSession.runtime);
		const streamingRenderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const streamingMode = new OpenTUIInteractiveMode(streamingHost, {
			createRenderer: async () => streamingRenderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
			initialMessages: ["steer now"],
		});
		await streamingMode.init();
		expect(streamingHost.prompts).toEqual([
			expect.objectContaining({
				text: "steer now",
				options: { source: "interactive", streamingBehavior: "steer" },
			}),
		]);
		await settle(streamingRenderer);
		expect(streamingRenderer.captureFrame()).toContain("Add instructions to the current task");

		streamingRenderer.input.pressEscape();
		await settle(streamingRenderer);
		expect(streamingSession.abort).not.toHaveBeenCalled();

		await streamingRenderer.input.typeText("queue next");
		streamingRenderer.input.pressEnter({ meta: true });
		await streamingMode.idle();
		expect(streamingHost.prompts.at(-1)).toEqual(
			expect.objectContaining({
				text: "queue next",
				options: { source: "interactive", streamingBehavior: "followUp" },
			}),
		);
		streamingSession.emit({ type: "queue_update", steering: ["steer now"], followUp: ["queue next"] });
		await streamingMode.idle();
		await settle(streamingRenderer);
		const queuedFrame = streamingRenderer.captureFrame();
		expect(queuedFrame).toContain("Current · steer now");
		expect(queuedFrame).toContain("Next · queue next");

		streamingRenderer.input.pressKey("c", { ctrl: true });
		await streamingMode.idle();
		expect(streamingSession.abort).toHaveBeenCalledOnce();
		streamingMode.stop();
	});

	test("dispatches steer and follow-up immediately without erasing newer input", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		let releaseFirst!: () => void;
		const firstTurn = new Promise<void>((resolve) => {
			releaseFirst = resolve;
		});
		vi.spyOn(host, "prompt").mockImplementation(async (runtime, text, options) => {
			host.prompts.push({ runtime, text, options });
			if (text === "start") {
				active.session.isStreaming = true;
				await firstTurn;
				active.session.isStreaming = false;
			}
		});
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();

		await renderer.input.typeText("start");
		renderer.input.pressEnter();
		await vi.waitFor(() => expect(host.prompts).toHaveLength(1));
		await renderer.input.typeText("steer now");
		renderer.input.pressEnter();
		await vi.waitFor(() => expect(host.prompts).toHaveLength(2));
		expect(host.prompts[1]).toEqual(
			expect.objectContaining({ text: "steer now", options: { source: "interactive", streamingBehavior: "steer" } }),
		);

		await renderer.input.typeText("queue next");
		renderer.input.pressEnter({ meta: true });
		await renderer.input.typeText("newer draft");
		await vi.waitFor(() => expect(host.prompts).toHaveLength(3));
		expect(host.prompts[2]).toEqual(
			expect.objectContaining({
				text: "queue next",
				options: { source: "interactive", streamingBehavior: "followUp" },
			}),
		);

		releaseFirst();
		await mode.idle();
		renderer.input.pressEnter();
		await mode.idle();
		expect(host.prompts.at(-1)).toEqual(expect.objectContaining({ text: "newer draft" }));
		mode.stop();
	});

	test("restores a failed submission to its originating conversation after a switch", async () => {
		const first = createRuntime();
		const second = createRuntime();
		const host = new FakeHost(first.runtime);
		let rejectPrompt!: (error: Error) => void;
		const pendingPrompt = new Promise<void>((_resolve, reject) => {
			rejectPrompt = reject;
		});
		vi.spyOn(host, "prompt").mockImplementationOnce(async (runtime, text, options) => {
			host.prompts.push({ runtime, text, options });
			await pendingPrompt;
		});
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		await renderer.input.typeText("restore me");
		renderer.input.pressEnter();
		await vi.waitFor(() => expect(host.prompts).toHaveLength(1));

		await host.hooks.beforeForegroundChange?.(first.runtime);
		host.current = second.runtime;
		await host.hooks.foregroundChanged?.(second.runtime);
		rejectPrompt(new Error("prompt failed"));
		await mode.idle();
		await host.hooks.beforeForegroundChange?.(second.runtime);
		host.current = first.runtime;
		await host.hooks.foregroundChanged?.(first.runtime);
		renderer.input.pressEnter();
		await mode.idle();
		expect(host.prompts.at(-1)).toEqual(expect.objectContaining({ runtime: first.runtime, text: "restore me" }));
		mode.stop();
	});

	test("merges a failed submission back without overwriting a newer draft", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		let rejectPrompt!: (error: Error) => void;
		const pendingPrompt = new Promise<void>((_resolve, reject) => {
			rejectPrompt = reject;
		});
		vi.spyOn(host, "prompt").mockImplementationOnce(async (runtime, text, options) => {
			host.prompts.push({ runtime, text, options });
			await pendingPrompt;
		});
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		await renderer.input.typeText("failed prompt");
		renderer.input.pressEnter();
		await vi.waitFor(() => expect(host.prompts).toHaveLength(1));
		await renderer.input.typeText("newer draft");
		rejectPrompt(new Error("prompt failed"));
		await mode.idle();
		renderer.input.pressEnter();
		await mode.idle();
		expect(host.prompts.at(-1)).toEqual(expect.objectContaining({ text: "failed prompt\n\nnewer draft" }));
		mode.stop();
	});

	test("waits for manual compaction before starting queued composer input", async () => {
		const active = createRuntime();
		active.session.isCompacting = true;
		let finishCompaction!: () => void;
		active.session.waitForCompaction = async () =>
			await new Promise<void>((resolve) => {
				finishCompaction = () => {
					active.session.isCompacting = false;
					resolve();
				};
			});
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		await renderer.input.typeText("after compact");
		renderer.input.pressEnter();
		await vi.waitFor(() => expect(finishCompaction).toBeTypeOf("function"));
		expect(host.prompts).toHaveLength(0);
		finishCompaction();
		await mode.idle();
		expect(host.prompts).toEqual([expect.objectContaining({ text: "after compact" })]);
		mode.stop();
	});

	test("keeps a prompt behind a preceding bash command in the same conversation", async () => {
		const active = createRuntime();
		let finishBash!: () => void;
		active.session.executeBash = async () => {
			active.session.isBashRunning = true;
			await new Promise<void>((resolve) => {
				finishBash = resolve;
			});
			active.session.isBashRunning = false;
			return { output: "done", exitCode: 0, cancelled: false, truncated: false };
		};
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		await renderer.input.typeText("! slow-command");
		renderer.input.pressEnter();
		await vi.waitFor(() => expect(finishBash).toBeTypeOf("function"));
		await renderer.input.typeText("use the command result");
		renderer.input.pressEnter();
		await settle(renderer);
		expect(host.prompts).toHaveLength(0);
		finishBash();
		await mode.idle();
		expect(host.prompts).toEqual([expect.objectContaining({ text: "use the command result" })]);
		mode.stop();
	});

	test("preserves a paused conversation's task-completed indicator while it runs in the background", async () => {
		const history = Array.from({ length: 30 }, (_, index) =>
			entry(assistantMessage(`historical response ${index}`), `history-${index}`),
		);
		const first = createRuntime(history, { sessionFile: "/tmp/first-background.jsonl" });
		const second = createRuntime([], { sessionFile: "/tmp/second-background.jsonl" });
		const host = new FakeHost(first.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 18 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		const focus = (mode as unknown as { transcriptFocus: { scrollByUser(delta: number): void } }).transcriptFocus;
		focus.scrollByUser(-8);

		await host.hooks.beforeForegroundChange?.(first.runtime);
		host.current = second.runtime;
		await host.hooks.foregroundChanged?.(second.runtime);
		await host.hooks.runCompleted?.(first.runtime, [assistantMessage("background result", "stop")]);
		host.hooks.runtimeDisposed?.(first.runtime);
		const resumedFirst = createRuntime(history, { sessionFile: "/tmp/first-background.jsonl" });
		await host.hooks.beforeForegroundChange?.(second.runtime);
		host.current = resumedFirst.runtime;
		await host.hooks.foregroundChanged?.(resumedFirst.runtime);
		await settle(renderer);
		expect(renderer.captureFrame()).toContain("Task completed");
		const banner = (mode as unknown as { transcriptUpdatesBanner: TextRenderable }).transcriptUpdatesBanner;
		const restingBannerAttributes = banner.attributes;
		await renderer.mouse.moveTo(banner.screenX + 1, banner.screenY);
		expect(banner.attributes).not.toBe(restingBannerAttributes);
		await renderer.mouse.moveTo(banner.screenX + 1, banner.screenY - 1);
		expect(banner.attributes).toBe(restingBannerAttributes);
		await renderer.mouse.pressDown(banner.screenX + 1, banner.screenY);
		await settle(renderer);
		expect(renderer.captureFrame()).toContain("Task completed");
		await renderer.mouse.release(banner.screenX + 1, banner.screenY);
		await settle(renderer);
		expect(renderer.captureFrame()).not.toContain("Task completed");
		mode.stop();
	});

	test("does not report a failed abort as stopped", async () => {
		const active = createRuntime();
		active.session.isStreaming = true;
		active.abort.mockRejectedValueOnce(new Error("abort failed"));
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		renderer.input.pressKey("c", { ctrl: true });
		await settle(renderer);
		const frame = renderer.captureFrame();
		expect(frame).toContain("abort failed");
		expect(frame).not.toContain("Stopped by user");
		mode.stop();
	});

	test.each([
		{ action: "approve" as const, down: 0 },
		{ action: "revise" as const, down: 1 },
		{ action: "cancel" as const, down: 2 },
	])("wires plan $action to the foreground session", async (scenario) => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 100, height: 28 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		const proposal = {
			id: `plan-${scenario.action}`,
			version: 2,
			content: "# Implement the migration",
			createdAt: new Date(0).toISOString(),
			sourceMessageId: "assistant-1",
		};
		active.emit({ type: "plan_proposed", proposal });
		active.emit({ type: "agent_settled" });
		expect(await renderer.waitForFrameText("Plan v2")).toContain("Plan v2");
		expect((mode as unknown as { overlayManager: { active: unknown } }).overlayManager.active).toBeFalsy();
		for (let index = 0; index < scenario.down; index++) renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		if (scenario.action === "revise") {
			await settle(renderer);
			expect(renderer.captureFrame()).toContain("Describe what should change");
			await renderer.input.typeText("Keep the public API smaller");
			renderer.input.pressEnter();
		}
		await mode.idle();
		await settle(renderer);
		if (scenario.action === "approve") {
			expect(active.session.approvePlan).toHaveBeenCalledWith(proposal.id);
		} else if (scenario.action === "revise") {
			expect(active.session.revisePlan).toHaveBeenCalledWith(proposal.id, "Keep the public API smaller");
		} else {
			expect(active.session.cancelPlan).toHaveBeenCalledWith(proposal.id);
		}
		mode.stop();
	});

	test("restores Plan revision feedback after switching conversations", async () => {
		const first = createRuntime([], { sessionFile: "/tmp/plan-first.jsonl", id: "plan-first" });
		const second = createRuntime([], { sessionFile: "/tmp/plan-second.jsonl", id: "plan-second" });
		const host = new FakeHost(first.runtime);
		const renderer = await createNativeTestRenderer({ width: 100, height: 28 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		const proposal = {
			id: "plan-switch-draft",
			version: 4,
			content: "# Keep the draft",
			createdAt: new Date(0).toISOString(),
			sourceMessageId: "assistant-plan-switch",
		};
		first.emit({ type: "plan_proposed", proposal });
		first.emit({ type: "agent_settled" });
		await renderer.waitForFrameText("Plan v4");
		renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		await renderer.input.typeText("Keep the adapter boundary");

		await host.hooks.beforeForegroundChange?.(first.runtime);
		host.current = second.runtime;
		await host.hooks.foregroundChanged?.(second.runtime);
		await mode.idle();
		await host.hooks.beforeForegroundChange?.(second.runtime);
		host.current = first.runtime;
		await host.hooks.foregroundChanged?.(first.runtime);
		await renderer.waitForFrameText("Plan v4");
		renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		await settle(renderer);

		expect(renderer.captureFrame()).toContain("Keep the adapter boundary");
		expect(first.session.revisePlan).not.toHaveBeenCalled();
		mode.stop();
	});

	test("opens /model as a non-modal quick picker above the composer", async () => {
		const active = createRuntime();
		const first = { provider: "test", id: "first", name: "First" };
		const second = { provider: "test", id: "second", name: "Second" };
		const setModel = vi.fn(async () => {});
		Object.assign(active.session, {
			model: first,
			modelRuntime: { getAvailable: async () => [first, second] },
			setModel,
		});
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 100, height: 28 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		await renderer.input.typeText("/model");
		renderer.input.pressEnter();
		const frame = await renderer.waitForFrameText("Select model");
		expect(frame).toContain("Search models");
		expect(frame).toContain("test/first");
		expect((mode as unknown as { overlayManager: { active: unknown } }).overlayManager.active).toBeFalsy();

		renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		await mode.idle();
		await settle(renderer);
		expect(setModel).toHaveBeenCalledWith(second);
		expect(renderer.captureFrame()).not.toContain("Select model");
		mode.stop();
	});

	test("dismisses a command quick picker when the foreground conversation changes", async () => {
		const first = createRuntime([], { sessionFile: "/tmp/picker-first.jsonl", id: "picker-first" });
		const second = createRuntime([], { sessionFile: "/tmp/picker-second.jsonl", id: "picker-second" });
		const models = [
			{ provider: "test", id: "first", name: "First" },
			{ provider: "test", id: "second", name: "Second" },
		];
		const setModel = vi.fn(async () => {});
		Object.assign(first.session, {
			model: models[0],
			modelRuntime: { getAvailable: async () => models },
			setModel,
		});
		const host = new FakeHost(first.runtime);
		const renderer = await createNativeTestRenderer({ width: 100, height: 28 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		await renderer.input.typeText("/model");
		renderer.input.pressEnter();
		await renderer.waitForFrameText("Select model");

		await host.hooks.beforeForegroundChange?.(first.runtime);
		host.current = second.runtime;
		await host.hooks.foregroundChanged?.(second.runtime);
		await mode.idle();
		await settle(renderer);

		expect(renderer.captureFrame()).not.toContain("Select model");
		expect(setModel).not.toHaveBeenCalled();
		mode.stop();
	});

	test("opens /settings as a main-area page and saves the draft with Ctrl+S", async () => {
		const root = mkdtempSync(join(tmpdir(), "bone-inline-settings-"));
		const active = createRuntime();
		const replaceScope = vi.fn(async () => {});
		const reloadSettings = vi.fn(async () => {});
		const reloadConfig = vi.fn(async () => {});
		Object.assign(active.runtime, { cwd: root });
		Object.assign(active.runtime.services, { agentDir: root });
		Object.assign(active.runtime.services.settingsManager, {
			getGlobalSettings: () => ({}),
			getProjectSettings: () => ({}),
			isProjectTrusted: () => true,
			replaceScope,
			reload: reloadSettings,
		});
		Object.assign(active.session, {
			modelRuntime: {
				getModelsJson: () => ({ providers: {} }),
				reloadConfig,
			},
		});
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 110, height: 32 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		try {
			await mode.init();
			await renderer.input.typeText("/settings");
			renderer.input.pressEnter();
			const settingsFrame = await renderer.waitForFrameText("Settings · Global");
			expect(settingsFrame).toContain("CONVERSATIONS");
			expect(settingsFrame).not.toContain("Ask anything");
			expect((mode as unknown as { overlayManager: { active: unknown } }).overlayManager.active).toBeFalsy();

			for (let index = 0; index < 3; index++) renderer.input.pressArrow("down");
			renderer.input.pressEnter();
			await renderer.waitForFrameText("Context & Delivery");
			for (let index = 0; index < 5; index++) renderer.input.pressArrow("down");
			renderer.input.pressEnter();
			await renderer.waitForFrameText("Hide thinking block");
			renderer.input.pressArrow("up");
			renderer.input.pressEnter();
			await renderer.waitForFrameText("Context & Delivery");
			renderer.input.pressKey("s", { ctrl: true });
			await mode.idle();
			await settle(renderer);

			expect(replaceScope).toHaveBeenCalledWith("global", { hideThinkingBlock: true });
			expect(reloadSettings).toHaveBeenCalledOnce();
			expect(reloadConfig).toHaveBeenCalledOnce();
			expect(renderer.captureFrame()).toContain("Ask anything");
			expect(renderer.captureFrame()).not.toContain("Settings · Global");
		} finally {
			mode.stop();
			rmSync(root, { recursive: true, force: true });
		}
	});

	test.each([
		{ command: "/tree", title: "Conversation tree", action: "navigate" as const },
		{ command: "/fork", title: "Fork conversation", action: "fork" as const },
	])("opens $command as a main-area history navigator", async (scenario) => {
		const user = entry(userMessage("Start here"), "history-user");
		const assistant = {
			...entry(assistantMessage("Current answer", "stop"), "history-assistant"),
			parentId: user.id,
		};
		const active = createRuntime([user, assistant]);
		const navigateTree = vi.fn(async () => ({ cancelled: false }));
		const fork = vi.fn(async () => ({ cancelled: false, selectedText: "Start here" }));
		Object.assign(active.session.sessionManager, {
			getBranch: () => [user, assistant],
			getLeafId: () => assistant.id,
			getTree: () => [
				{
					entry: user,
					children: [{ entry: assistant, children: [] }],
				},
			],
		});
		Object.assign(active.session, { navigateTree });
		Object.assign(active.runtime, { fork });
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 100, height: 28 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		await renderer.input.typeText(scenario.command);
		renderer.input.pressEnter();
		const frame = await renderer.waitForFrameText(scenario.title);
		expect(frame).toContain("CONVERSATIONS");
		expect(frame).not.toContain("Ask anything");
		expect((mode as unknown as { overlayManager: { active: unknown } }).overlayManager.active).toBeFalsy();

		renderer.input.pressArrow("up");
		renderer.input.pressEnter();
		await mode.idle();
		await settle(renderer);
		if (scenario.action === "navigate") expect(navigateTree).toHaveBeenCalledWith(user.id);
		else expect(fork).toHaveBeenCalledWith(user.id);
		expect(renderer.captureFrame()).toContain("Ask anything");
		mode.stop();
	});

	test("opens /scoped-models as one non-modal multi-select picker", async () => {
		const active = createRuntime();
		const first = { provider: "test", id: "first", name: "First" };
		const second = { provider: "test", id: "second", name: "Second" };
		const setScopedModels = vi.fn();
		Object.assign(active.session, {
			scopedModels: [{ model: first }],
			modelRuntime: { getAvailable: async () => [first, second] },
			setScopedModels,
		});
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 100, height: 28 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		await renderer.input.typeText("/scoped-models");
		renderer.input.pressEnter();
		const frame = await renderer.waitForFrameText("Model cycling scope");
		expect(frame).toContain("[x] First");
		expect(frame).toContain("1 selected");
		expect((mode as unknown as { overlayManager: { active: unknown } }).overlayManager.active).toBeFalsy();

		renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		renderer.input.pressKey("s", { ctrl: true });
		await mode.idle();
		await settle(renderer);
		expect(setScopedModels).toHaveBeenCalledWith([{ model: first }, { model: second }]);
		expect(renderer.captureFrame()).not.toContain("Model cycling scope");
		mode.stop();
	});

	test("opens /import as an inline prompt and restores composer focus", async () => {
		const active = createRuntime();
		const importFromJsonl = vi.fn(async () => ({ cancelled: false }));
		Object.assign(active.runtime, { importFromJsonl });
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 100, height: 28 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		await renderer.input.typeText("/import");
		renderer.input.pressEnter();
		const frame = await renderer.waitForFrameText("Import JSONL");
		expect(frame).toContain("Import JSONL");
		expect((mode as unknown as { overlayManager: { active: unknown } }).overlayManager.active).toBeFalsy();
		await renderer.input.typeText("/tmp/release-work.jsonl");
		renderer.input.pressEnter();
		await mode.idle();
		await settle(renderer);
		expect(importFromJsonl).toHaveBeenCalledWith("/tmp/release-work.jsonl");
		expect(renderer.captureFrame()).not.toContain("Enter submit · Esc cancel");
		mode.stop();
	});

	test("collects structured answers in one non-modal panel and restores composer focus", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 110, height: 32 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		const request = {
			id: "question-1",
			toolCallId: "tool-1",
			createdAt: new Date(0).toISOString(),
			questions: [
				{
					header: "Runtime",
					question: "Which runtime?",
					options: [
						{ label: "Bun", description: "Use Bun only" },
						{ label: "Node", description: "Keep Node support" },
					],
				},
				{
					header: "Scope",
					question: "What should be included?",
					options: [
						{ label: "TUI", description: "Migrate the terminal UI" },
						{ label: "RPC", description: "Migrate RPC surfaces" },
					],
					multiSelect: true,
				},
				{
					header: "Notes",
					question: "Any final constraint?",
					options: [
						{ label: "None", description: "No extra constraint" },
						{ label: "Tests", description: "Prioritize tests" },
					],
				},
			],
		};
		active.emit({ type: "question_asked", request });
		const questionFrame = await renderer.waitForFrameText("Which runtime?");
		expect(questionFrame).toContain("Agent needs your input");
		expect(questionFrame).toContain("[1 Runtime]  2 Scope  3 Notes  Review");
		expect((mode as unknown as { overlayManager: { active: unknown } }).overlayManager.active).toBeFalsy();
		renderer.input.pressEnter();
		renderer.input.pressArrow("right");
		renderer.input.pressEnter();
		renderer.input.pressArrow("right");
		for (let index = 0; index < 2; index++) renderer.input.pressArrow("down");
		await settle(renderer);
		await renderer.input.typeText("Do not add configurable keys");
		renderer.input.pressEnter();
		renderer.input.pressArrow("right");
		await renderer.waitForFrameText("Review answers and add an overall note");
		renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		await renderer.waitForFrameText("Write overall note");
		await renderer.input.typeText("Apply these answers to the whole migration");
		renderer.input.pressEnter();
		renderer.input.pressArrow("up");
		renderer.input.pressEnter();

		await vi.waitFor(() => expect(active.session.answerQuestion).toHaveBeenCalledOnce());
		await mode.idle();
		await settle(renderer);
		expect(active.session.answerQuestion).toHaveBeenCalledWith(
			request.id,
			[
				{ questionIndex: 0, question: "Which runtime?", kind: "option", answer: "Bun" },
				{
					questionIndex: 1,
					question: "What should be included?",
					kind: "multi",
					answer: null,
					selected: ["TUI"],
				},
				{
					questionIndex: 2,
					question: "Any final constraint?",
					kind: "note",
					answer: null,
					notes: "Do not add configurable keys",
				},
			],
			"Apply these answers to the whole migration",
		);

		await renderer.input.typeText("focus restored");
		renderer.input.pressEnter();
		await mode.idle();
		expect(host.prompts).toEqual([expect.objectContaining({ text: "focus restored" })]);
		mode.stop();
	});

	test("routes keys to a structured question that arrives while the sidebar is focused", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 120, height: 28 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		const paneFocus = (
			mode as unknown as { paneFocus: { focus(pane: "sidebar" | "composer"): void; focusedPane: string } }
		).paneFocus;
		paneFocus.focus("sidebar");
		expect(paneFocus.focusedPane).toBe("sidebar");

		const request = {
			id: "question-from-sidebar",
			toolCallId: "tool-from-sidebar",
			createdAt: new Date(0).toISOString(),
			questions: [
				{
					header: "Runtime",
					question: "Which runtime?",
					options: [
						{ label: "Bun", description: "Use Bun only" },
						{ label: "Node", description: "Keep Node support" },
					],
				},
			],
		};
		active.emit({ type: "question_asked", request });
		await renderer.waitForFrameText("Which runtime?");
		renderer.input.pressEnter();
		renderer.input.pressKey("s", { ctrl: true });

		await mode.idle();
		expect(active.session.answerQuestion).toHaveBeenCalledWith(request.id, [
			{ questionIndex: 0, question: "Which runtime?", kind: "option", answer: "Bun" },
		]);
		mode.stop();
	});

	test("cancels a pending structured question when the questionnaire is dismissed", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			installSignalHandlers: false,
		});
		await mode.init();
		const request = {
			id: "question-cancel",
			toolCallId: "tool-cancel",
			createdAt: new Date(0).toISOString(),
			questions: [
				{
					header: "Choice",
					question: "Continue?",
					options: [
						{ label: "Yes", description: "Continue" },
						{ label: "No", description: "Stop" },
					],
				},
			],
		};
		active.emit({ type: "question_asked", request });
		await settle(renderer);
		renderer.input.pressEscape();
		await mode.idle();
		expect(active.session.cancelQuestion).toHaveBeenCalledWith(request.id, "user");
		mode.stop();
	});

	test("closes a question dialog on an external abort and continues draining session events", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory(renderer);
		const handleEvent = vi.spyOn(transcriptFactory, "handleEvent");
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			createTranscriptFactory: () => transcriptFactory,
			installSignalHandlers: false,
		});
		await mode.init();
		const request = {
			id: "question-external-abort",
			toolCallId: "tool-external-abort",
			createdAt: new Date(0).toISOString(),
			questions: [
				{
					header: "Runtime",
					question: "Which runtime?",
					options: [
						{ label: "Bun", description: "Use Bun only" },
						{ label: "Node", description: "Keep Node support" },
					],
				},
			],
		};
		active.emit({ type: "question_asked", request });
		await vi.waitFor(() => expect(renderer.captureFrame()).toContain("Which runtime?"));

		active.emit({ type: "question_cancelled", requestId: request.id, reason: "abort" });
		active.emit({ type: "message_start", message: userMessage("event after question abort") });
		await mode.idle();
		await settle(renderer);

		expect(renderer.captureFrame()).not.toContain("Which runtime?");
		expect(handleEvent).toHaveBeenCalledWith(
			expect.objectContaining({ type: "message_start", message: expect.objectContaining({ role: "user" }) }),
		);
		expect(active.session.answerQuestion).not.toHaveBeenCalled();
		expect(active.session.cancelQuestion).not.toHaveBeenCalled();
		mode.stop();
	});

	test("closes a plan dialog on an external decision and continues draining session events", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		const renderer = await createNativeTestRenderer({ width: 100, height: 28 });
		const transcriptFactory = new OpenTUITranscriptFactory(renderer);
		const handleEvent = vi.spyOn(transcriptFactory, "handleEvent");
		const mode = new OpenTUIInteractiveMode(host, {
			createRenderer: async () => renderer,
			createMemoryRuntime: () => new FakeMemoryRuntime(),
			createTranscriptFactory: () => transcriptFactory,
			installSignalHandlers: false,
		});
		await mode.init();
		const proposal = {
			id: "plan-external-decision",
			version: 3,
			content: "# Finish the OpenTUI migration",
			createdAt: new Date(0).toISOString(),
			sourceMessageId: "assistant-external-decision",
		};
		active.emit({ type: "plan_proposed", proposal });
		active.emit({ type: "agent_settled" });
		await vi.waitFor(() => expect(renderer.captureFrame()).toContain("Plan v3"));

		active.emit({ type: "plan_decided", proposal, decision: "approved" });
		active.emit({ type: "message_start", message: userMessage("event after plan decision") });
		await mode.idle();
		await settle(renderer);

		expect(renderer.captureFrame()).not.toContain("Plan v3");
		expect(handleEvent).toHaveBeenCalledWith(
			expect.objectContaining({ type: "message_start", message: expect.objectContaining({ role: "user" }) }),
		);
		expect(active.session.approvePlan).not.toHaveBeenCalled();
		expect(active.session.revisePlan).not.toHaveBeenCalled();
		expect(active.session.cancelPlan).not.toHaveBeenCalled();
		mode.stop();
	});
});
