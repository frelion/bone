import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AgentMessage } from "@frelion/bone-agent-core";
import type { AssistantMessage } from "@frelion/bone-ai/compat";
import { type BoneTestRenderer, createBoneTestRenderer } from "@frelion/bone-tui";
import { beforeEach, describe, expect, test, vi } from "vitest";
import type { AgentSessionEvent, AgentSessionEventListener, PromptOptions } from "../src/core/agent-session.ts";
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
	OpenTUIInteractiveMode,
	type OpenTUISessionHostContract,
} from "../src/modes/interactive/opentui-interactive-mode.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

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
	bindExtensions(): Promise<void>;
	parkExtensionUI(): void;
	approvePlan(proposalId: string): Promise<void>;
	revisePlan(proposalId: string, feedback: string): Promise<void>;
	cancelPlan(proposalId: string): void;
	answerQuestion(requestId: string, answers: QuestionAnswer[]): void;
	cancelQuestion(requestId: string, reason: "user" | "abort" | "client_disconnect" | "no_ui"): void;
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
		{ revision: number; liveEvents: AgentSessionEvent[]; liveEventRevisions: number[] }
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

	getRuntimeStreamSnapshot(runtime: AgentSessionRuntime): RuntimeStreamSnapshot {
		const state = this.streamState.get(runtime) ?? { revision: 0, liveEvents: [], liveEventRevisions: [] };
		return {
			revision: state.revision,
			generationId: undefined,
			liveEvents: state.liveEvents.slice(),
			liveEventEnvelopes: state.liveEvents.map((event, index) => ({
				runtime,
				revision: state.liveEventRevisions[index] ?? state.revision,
				generationId: undefined,
				event,
			})),
		};
	}

	subscribeRuntime(runtime: AgentSessionRuntime, listener: (envelope: RuntimeEventEnvelope) => void): () => void {
		const state = this.streamState.get(runtime) ?? { revision: 0, liveEvents: [], liveEventRevisions: [] };
		this.streamState.set(runtime, state);
		return runtime.session.subscribe((event) => {
			state.revision++;
			if (event.type === "agent_start") {
				state.liveEvents = [];
				state.liveEventRevisions = [];
			}
			state.liveEvents.push(event);
			state.liveEventRevisions.push(state.revision);
			listener({ runtime, revision: state.revision, generationId: undefined, event });
			if (event.type === "agent_settled") {
				state.liveEvents = [];
				state.liveEventRevisions = [];
			}
		});
	}
}

async function settle(renderer: BoneTestRenderer): Promise<void> {
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
	test("renders initial history and routes the fixed composer submit action", async () => {
		const session = createRuntime([
			entry(userMessage("earlier prompt"), "one"),
			entry(assistantMessage("earlier answer", "stop"), "two"),
		]);
		const host = new FakeHost(session.runtime);
		const renderer = await createBoneTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory();
		vi.spyOn(transcriptFactory, "createSessionEntry").mockImplementation(async (sessionEntry) => ({
			key: sessionEntry.id,
			view: {
				mount: (context) =>
					context.createText({
						content:
							sessionEntry.type === "message" ? JSON.stringify(sessionEntry.message.content) : sessionEntry.type,
					}),
			},
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
		const renderer = await createBoneTestRenderer({ width: 90, height: 24 });
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
		const renderer = await createBoneTestRenderer({ width: 90, height: 28 });
		const transcriptFactory = new OpenTUITranscriptFactory();
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
			result: { content: [{ type: "text", text: "final chunk" }] },
			isError: false,
		});
		await mode.idle();
		await settle(renderer);

		let frame = renderer.captureFrame();
		expect(frame).toContain("✓ Worked for 1s · 1 tool calls");
		expect(frame).not.toContain("final chunk");
		renderer.input.pressKey("o", { ctrl: true });
		await settle(renderer);
		frame = renderer.captureFrame();
		expect(frame).toContain("read · complete");
		expect(frame).toContain("final chunk");
		mode.stop();
	});

	test("replays runtime events emitted while transcript history is loading", async () => {
		const active = createRuntime([entry(userMessage("earlier prompt"), "one")]);
		const host = new FakeHost(active.runtime);
		const renderer = await createBoneTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory();
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
				return await new OpenTUITranscriptFactory().createSessionEntries(entries);
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

	test("rebinds foreground subscriptions and handles fixed interrupt and shutdown keys", async () => {
		const first = createRuntime([entry(userMessage("first session"), "first")]);
		const second = createRuntime([entry(userMessage("second session"), "second")]);
		const host = new FakeHost(first.runtime);
		const renderer = await createBoneTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory();
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
		renderer.input.pressKey("c", { ctrl: true });

		second.session.isStreaming = true;
		renderer.input.pressKey("c", { ctrl: true });
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
			const renderer = await createBoneTestRenderer({ width: 90, height: 24 });
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
		const renderer = await createBoneTestRenderer({ width: 100, height: 28 });
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
		await settle(renderer);
		expect(memory.search).toHaveBeenCalledWith("indexed", expect.arrayContaining([visible]));
		expect(host.getSessionSummaries).toHaveBeenCalledWith([indexed.path]);
		expect(renderer.captureFrame()).toContain("Indexed result");

		renderer.input.pressEscape();
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
		const renderer = await createBoneTestRenderer({ width: 90, height: 24 });
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
		const streamingRenderer = await createBoneTestRenderer({ width: 90, height: 24 });
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
		streamingMode.stop();
	});

	test("wires plan approval, revision, and cancellation to the foreground session", async () => {
		const scenarios = [
			{ action: "approve" as const, down: 0 },
			{ action: "revise" as const, down: 1 },
			{ action: "cancel" as const, down: 2 },
		];
		for (const scenario of scenarios) {
			const active = createRuntime();
			const host = new FakeHost(active.runtime);
			const renderer = await createBoneTestRenderer({ width: 100, height: 28 });
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
			await settle(renderer);
			expect(renderer.captureFrame()).toContain("Plan v2");
			for (let index = 0; index < scenario.down; index++) renderer.input.pressArrow("down");
			renderer.input.pressEnter();
			if (scenario.action === "revise") {
				await settle(renderer);
				expect(renderer.captureFrame()).toContain("Revise plan v2");
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
		}
	});

	test("collects complete option, custom, and multi-select answers and restores composer focus", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		const renderer = await createBoneTestRenderer({ width: 110, height: 32 });
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
		await settle(renderer);
		renderer.input.pressEnter();

		await settle(renderer);
		renderer.input.pressEnter();
		await settle(renderer);
		for (let index = 0; index < 3; index++) renderer.input.pressArrow("down");
		renderer.input.pressEnter();

		await settle(renderer);
		for (let index = 0; index < 2; index++) renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		await settle(renderer);
		await renderer.input.typeText("Do not add configurable keys");
		renderer.input.pressEnter();

		await mode.idle();
		await settle(renderer);
		expect(active.session.answerQuestion).toHaveBeenCalledWith(request.id, [
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
				kind: "custom",
				answer: "Do not add configurable keys",
			},
		]);

		await renderer.input.typeText("focus restored");
		renderer.input.pressEnter();
		await mode.idle();
		expect(host.prompts).toEqual([expect.objectContaining({ text: "focus restored" })]);
		mode.stop();
	});

	test("cancels a pending structured question when the questionnaire is dismissed", async () => {
		const active = createRuntime();
		const host = new FakeHost(active.runtime);
		const renderer = await createBoneTestRenderer({ width: 90, height: 24 });
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
		const renderer = await createBoneTestRenderer({ width: 90, height: 24 });
		const transcriptFactory = new OpenTUITranscriptFactory();
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
		const renderer = await createBoneTestRenderer({ width: 100, height: 28 });
		const transcriptFactory = new OpenTUITranscriptFactory();
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
