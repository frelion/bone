import { existsSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AgentMessage } from "@frelion/bone-agent-core";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AgentSessionEvent } from "../src/core/agent-session.ts";
import {
	type CreateAgentSessionRuntimeFactory,
	createAgentSessionFromServices,
	createAgentSessionRuntime,
	createAgentSessionServices,
} from "../src/core/agent-session-runtime.ts";
import { getLastActiveConversation, rememberLastActiveConversation } from "../src/core/conversation-state.ts";
import { InteractiveSessionHost } from "../src/core/interactive-session-host.ts";
import { type SessionEntry, SessionManager } from "../src/core/session-manager.ts";
import { assistantMsg, userMsg } from "./utilities.ts";

type SessionInternals = {
	_emit(event: AgentSessionEvent): void;
	_emitPersistedEntries(entries: readonly SessionEntry[]): Promise<void>;
	_emitAgentSettled(): Promise<void>;
	_isAgentRunActive: boolean;
};

function createPersistedSession(cwd: string, text: string): SessionManager {
	const sessionManager = SessionManager.create(cwd);
	sessionManager.appendMessage(userMsg(text));
	sessionManager.appendMessage(assistantMsg(`${text} response`));
	return sessionManager;
}

function messageEntry(message: AgentMessage, id: string): SessionEntry {
	return { type: "message", id, parentId: null, timestamp: new Date(0).toISOString(), message };
}

describe("InteractiveSessionHost", () => {
	const tempDirs: string[] = [];

	afterEach(() => {
		for (const tempDir of tempDirs.splice(0)) {
			if (existsSync(tempDir)) {
				rmSync(tempDir, { recursive: true, force: true });
			}
		}
	});

	it("closes an idle session when switching and reopens it from its JSONL history", async () => {
		const tempDir = join(tmpdir(), `bone-session-host-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(tempDir);
		mkdirSync(tempDir, { recursive: true });

		const firstSession = createPersistedSession(tempDir, "first session");
		const secondSession = createPersistedSession(tempDir, "second session");
		const firstPath = firstSession.getSessionFile();
		const secondPath = secondSession.getSessionFile();
		if (!firstPath || !secondPath) {
			throw new Error("expected persisted session files");
		}

		const factory: CreateAgentSessionRuntimeFactory = async ({
			cwd,
			agentDir,
			sessionManager,
			sessionStartEvent,
		}) => {
			const services = await createAgentSessionServices({
				cwd,
				agentDir,
				resourceLoaderOptions: {
					noExtensions: true,
					noSkills: true,
					noPromptTemplates: true,
					noThemes: true,
				},
			});
			return {
				...(await createAgentSessionFromServices({
					services,
					sessionManager,
					sessionStartEvent,
					noTools: "all",
				})),
				services,
				diagnostics: services.diagnostics,
			};
		};

		const initialRuntime = await createAgentSessionRuntime(factory, {
			cwd: tempDir,
			agentDir: tempDir,
			sessionManager: SessionManager.open(firstPath),
		});
		const dispose = vi.spyOn(initialRuntime, "dispose");
		const host = new InteractiveSessionHost(initialRuntime, factory);

		await host.activate(secondPath);

		expect(dispose).toHaveBeenCalledOnce();
		expect(host.current.session.sessionFile).toBe(secondPath);
		expect((await host.list()).find((session) => session.path === secondPath)?.state).toBe("foreground");

		await host.activate(firstPath);

		expect(host.current).not.toBe(initialRuntime);
		expect(host.current.session.messages.some((message) => message.role === "assistant")).toBe(true);
		await host.disposeAll();
	});

	it("keeps a running session alive in the background until it settles", async () => {
		const tempDir = join(tmpdir(), `bone-session-host-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(tempDir);
		mkdirSync(tempDir, { recursive: true });

		const firstSession = createPersistedSession(tempDir, "first session");
		const secondSession = createPersistedSession(tempDir, "second session");
		const firstPath = firstSession.getSessionFile();
		const secondPath = secondSession.getSessionFile();
		if (!firstPath || !secondPath) {
			throw new Error("expected persisted session files");
		}

		const factory: CreateAgentSessionRuntimeFactory = async ({
			cwd,
			agentDir,
			sessionManager,
			sessionStartEvent,
		}) => {
			const services = await createAgentSessionServices({
				cwd,
				agentDir,
				resourceLoaderOptions: {
					noExtensions: true,
					noSkills: true,
					noPromptTemplates: true,
					noThemes: true,
				},
			});
			return {
				...(await createAgentSessionFromServices({
					services,
					sessionManager,
					sessionStartEvent,
					noTools: "all",
				})),
				services,
				diagnostics: services.diagnostics,
			};
		};

		const initialRuntime = await createAgentSessionRuntime(factory, {
			cwd: tempDir,
			agentDir: tempDir,
			sessionManager: SessionManager.open(firstPath),
		});
		const dispose = vi.spyOn(initialRuntime, "dispose");
		const host = new InteractiveSessionHost(initialRuntime, factory);
		const runtimeDisposed = vi.fn();
		host.setHooks({ runtimeDisposed });
		const internals = initialRuntime.session as unknown as SessionInternals;
		internals._isAgentRunActive = true;

		await host.activate(secondPath);

		expect(dispose).not.toHaveBeenCalled();
		expect((await host.list()).find((session) => session.path === firstPath)?.state).toBe("background-running");

		await internals._emitAgentSettled();
		await host.waitForTransitions();

		expect(dispose).toHaveBeenCalledOnce();
		expect(runtimeDisposed).toHaveBeenCalledWith(initialRuntime);
		expect((await host.list()).find((session) => session.path === firstPath)?.state).toBe("cold");
		await host.disposeAll();
	});

	it("routes prompts independently to the runtime captured at submission", async () => {
		const tempDir = join(tmpdir(), `bone-session-host-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(tempDir);
		mkdirSync(tempDir, { recursive: true });

		const firstSession = createPersistedSession(tempDir, "first session");
		const secondSession = createPersistedSession(tempDir, "second session");
		const firstPath = firstSession.getSessionFile();
		const secondPath = secondSession.getSessionFile();
		if (!firstPath || !secondPath) throw new Error("expected persisted session files");

		const factory: CreateAgentSessionRuntimeFactory = async ({
			cwd,
			agentDir,
			sessionManager,
			sessionStartEvent,
		}) => {
			const services = await createAgentSessionServices({
				cwd,
				agentDir,
				resourceLoaderOptions: {
					noExtensions: true,
					noSkills: true,
					noPromptTemplates: true,
					noThemes: true,
				},
			});
			return {
				...(await createAgentSessionFromServices({
					services,
					sessionManager,
					sessionStartEvent,
					noTools: "all",
				})),
				services,
				diagnostics: services.diagnostics,
			};
		};

		const runtimeA = await createAgentSessionRuntime(factory, {
			cwd: tempDir,
			agentDir: tempDir,
			sessionManager: SessionManager.open(firstPath),
		});
		const host = new InteractiveSessionHost(runtimeA, factory);
		let finishA: () => void = () => {};
		const promptA = vi.spyOn(runtimeA.session, "prompt").mockImplementation(
			async () =>
				await new Promise<void>((resolve) => {
					finishA = resolve;
				}),
		);

		const submittedA = host.prompt(runtimeA, "message for A");
		await vi.waitFor(() => expect(promptA).toHaveBeenCalledWith("message for A", undefined));
		await host.activate(secondPath);

		const runtimeB = host.current;
		const promptB = vi.spyOn(runtimeB.session, "prompt").mockResolvedValue();
		await host.prompt(runtimeB, "message for B");

		expect(promptB).toHaveBeenCalledWith("message for B", undefined);
		expect((await host.list()).find((session) => session.path === firstPath)?.state).toBe("background-running");
		finishA();
		await submittedA;
		await host.disposeAll();
	});

	it("delivers steer and follow-up prompts without waiting for the active prompt tail", async () => {
		const tempDir = join(tmpdir(), `bone-session-host-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(tempDir);
		mkdirSync(tempDir, { recursive: true });
		const manager = createPersistedSession(tempDir, "active session");
		const sessionPath = manager.getSessionFile();
		if (!sessionPath) throw new Error("expected persisted session file");
		const factory: CreateAgentSessionRuntimeFactory = async ({ cwd, agentDir, sessionManager }) => {
			const services = await createAgentSessionServices({
				cwd,
				agentDir,
				resourceLoaderOptions: { noExtensions: true, noSkills: true, noPromptTemplates: true, noThemes: true },
			});
			return {
				...(await createAgentSessionFromServices({ services, sessionManager, noTools: "all" })),
				services,
				diagnostics: services.diagnostics,
			};
		};
		const runtime = await createAgentSessionRuntime(factory, {
			cwd: tempDir,
			agentDir: tempDir,
			sessionManager: SessionManager.open(sessionPath),
		});
		const host = new InteractiveSessionHost(runtime, factory);
		const internals = runtime.session as unknown as SessionInternals;
		internals._isAgentRunActive = true;
		let releaseActive!: () => void;
		const activePrompt = new Promise<void>((resolve) => {
			releaseActive = resolve;
		});
		const prompt = vi.spyOn(runtime.session, "prompt").mockImplementation(async (text) => {
			if (text === "active") await activePrompt;
		});

		const running = host.prompt(runtime, "active");
		await vi.waitFor(() => expect(prompt).toHaveBeenCalledTimes(1));
		const steer = host.prompt(runtime, "steer", { streamingBehavior: "steer" });
		const followUp = host.prompt(runtime, "follow up", { streamingBehavior: "followUp" });
		await vi.waitFor(() => expect(prompt).toHaveBeenCalledTimes(3));
		expect(prompt).toHaveBeenNthCalledWith(2, "steer", { streamingBehavior: "steer" });
		expect(prompt).toHaveBeenNthCalledWith(3, "follow up", { streamingBehavior: "followUp" });
		await Promise.all([steer, followUp]);
		internals._isAgentRunActive = false;
		releaseActive();
		await running;
		await host.disposeAll();
	});

	it("keeps lifecycle ordering after an immediate steer fails", async () => {
		const tempDir = join(tmpdir(), `bone-session-host-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(tempDir);
		mkdirSync(tempDir, { recursive: true });
		const manager = createPersistedSession(tempDir, "active session");
		const sessionPath = manager.getSessionFile();
		if (!sessionPath) throw new Error("expected persisted session file");
		const factory: CreateAgentSessionRuntimeFactory = async ({ cwd, agentDir, sessionManager }) => {
			const services = await createAgentSessionServices({
				cwd,
				agentDir,
				resourceLoaderOptions: { noExtensions: true, noSkills: true, noPromptTemplates: true, noThemes: true },
			});
			return {
				...(await createAgentSessionFromServices({ services, sessionManager, noTools: "all" })),
				services,
				diagnostics: services.diagnostics,
			};
		};
		const runtime = await createAgentSessionRuntime(factory, {
			cwd: tempDir,
			agentDir: tempDir,
			sessionManager: SessionManager.open(sessionPath),
		});
		const host = new InteractiveSessionHost(runtime, factory);
		const internals = runtime.session as unknown as SessionInternals;
		internals._isAgentRunActive = true;
		let releaseActive!: () => void;
		const activePrompt = new Promise<void>((resolve) => {
			releaseActive = resolve;
		});
		const prompt = vi.spyOn(runtime.session, "prompt").mockImplementation(async (text) => {
			if (text === "active") await activePrompt;
			if (text === "bad steer") throw new Error("steer rejected");
		});

		const running = host.prompt(runtime, "active");
		await vi.waitFor(() => expect(prompt).toHaveBeenCalledTimes(1));
		await expect(host.prompt(runtime, "bad steer", { streamingBehavior: "steer" })).rejects.toThrow("steer rejected");
		internals._isAgentRunActive = false;
		const after = host.prompt(runtime, "after active run");
		await new Promise<void>((resolve) => setImmediate(resolve));
		expect(prompt).toHaveBeenCalledTimes(2);
		releaseActive();
		await Promise.all([running, after]);
		expect(prompt).toHaveBeenNthCalledWith(3, "after active run", undefined);
		await host.disposeAll();
	});

	it("keeps an unflushed first turn visible after switching to another session", async () => {
		const tempDir = join(tmpdir(), `bone-session-host-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(tempDir);
		mkdirSync(tempDir, { recursive: true });

		const firstSession = SessionManager.create(tempDir);
		firstSession.appendMessage(userMsg("first turn is still streaming"));
		const firstPath = firstSession.getSessionFile();
		const secondSession = createPersistedSession(tempDir, "second session");
		const secondPath = secondSession.getSessionFile();
		if (!firstPath || !secondPath) {
			throw new Error("expected persisted session paths");
		}
		expect(existsSync(firstPath)).toBe(false);

		const factory: CreateAgentSessionRuntimeFactory = async ({
			cwd,
			agentDir,
			sessionManager,
			sessionStartEvent,
		}) => {
			const services = await createAgentSessionServices({
				cwd,
				agentDir,
				resourceLoaderOptions: {
					noExtensions: true,
					noSkills: true,
					noPromptTemplates: true,
					noThemes: true,
				},
			});
			return {
				...(await createAgentSessionFromServices({
					services,
					sessionManager,
					sessionStartEvent,
					noTools: "all",
				})),
				services,
				diagnostics: services.diagnostics,
			};
		};

		const initialRuntime = await createAgentSessionRuntime(factory, {
			cwd: tempDir,
			agentDir: tempDir,
			sessionManager: firstSession,
		});
		const host = new InteractiveSessionHost(initialRuntime, factory);
		const internals = initialRuntime.session as unknown as SessionInternals;
		internals._isAgentRunActive = true;

		await host.activate(secondPath);

		const backgroundFirstSession = (await host.list()).find((session) => session.path === firstPath);
		expect(backgroundFirstSession).toMatchObject({
			state: "background-running",
			firstMessage: "first turn is still streaming",
		});
		await host.disposeAll();
	});

	it("publishes throttled live preview, count, and throughput presentation", async () => {
		vi.useFakeTimers();
		vi.setSystemTime(new Date("2026-07-22T12:00:00.000Z"));
		const tempDir = join(tmpdir(), `bone-session-host-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(tempDir);
		mkdirSync(tempDir, { recursive: true });
		const sessionManager = createPersistedSession(tempDir, "live session");
		const sessionPath = sessionManager.getSessionFile();
		if (!sessionPath) throw new Error("expected persisted session file");
		const factory: CreateAgentSessionRuntimeFactory = async ({ cwd, agentDir, sessionManager: manager }) => {
			const services = await createAgentSessionServices({
				cwd,
				agentDir,
				resourceLoaderOptions: { noExtensions: true, noSkills: true, noPromptTemplates: true, noThemes: true },
			});
			return {
				...(await createAgentSessionFromServices({ services, sessionManager: manager, noTools: "all" })),
				services,
				diagnostics: services.diagnostics,
			};
		};
		const runtime = await createAgentSessionRuntime(factory, {
			cwd: tempDir,
			agentDir: tempDir,
			sessionManager: SessionManager.open(sessionPath),
		});
		const host = new InteractiveSessionHost(runtime, factory);
		const stateChanged = vi.fn();
		host.setHooks({ stateChanged });
		const getEntries = vi.spyOn(runtime.session.sessionManager, "getEntries");
		getEntries.mockClear();
		const internals = runtime.session as unknown as SessionInternals;
		const partial = assistantMsg("Streaming answer");
		const streamedEvents: AgentSessionEvent[] = [];
		const revisions: number[] = [];
		host.subscribeRuntime(runtime, (envelope) => {
			streamedEvents.push(envelope.event);
			revisions.push(envelope.revision);
		});

		internals._emit({ type: "agent_start" });
		expect(stateChanged).toHaveBeenCalledOnce();
		internals._emit({ type: "message_start", message: assistantMsg("") });
		internals._emit({
			type: "message_update",
			message: partial,
			assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "Streaming answer", partial },
		});
		await vi.advanceTimersByTimeAsync(249);
		expect(stateChanged).toHaveBeenCalledOnce();
		await vi.advanceTimersByTimeAsync(1);
		expect(stateChanged).toHaveBeenCalledTimes(2);
		expect(getEntries).not.toHaveBeenCalled();

		expect(host.getSessionPresentation(sessionPath)).toMatchObject({
			state: "foreground",
			livePreview: "Streaming answer",
			messageCount: 3,
		});
		expect(host.getSessionPresentation(sessionPath).throughputTokensPerSecond).toBeGreaterThan(0);
		const snapshot = host.getRuntimeStreamSnapshot(runtime);
		expect(snapshot.liveEvents.map((event) => event.type)).toEqual([
			"agent_start",
			"message_start",
			"message_update",
		]);
		expect(revisions).toEqual([1, 2, 3]);
		expect(streamedEvents).toHaveLength(3);
		const content = partial.content.find((part) => part.type === "text");
		if (content?.type === "text") content.text = "mutated after emission";
		const replayedUpdate = host
			.getRuntimeStreamSnapshot(runtime)
			.liveEvents.find((event) => event.type === "message_update");
		const replayedText =
			replayedUpdate?.type === "message_update"
				? replayedUpdate.assistantMessageEvent.partial.content
						.filter((part) => part.type === "text")
						.map((part) => part.text)
						.join("")
				: "";
		expect(replayedText).toBe("Streaming answer");
		await internals._emitPersistedEntries([messageEntry(partial, "streamed-message")]);
		expect(host.getRuntimeStreamSnapshot(runtime).liveEvents.map((event) => event.type)).toEqual(["agent_start"]);

		internals._emit({ type: "agent_end", messages: [], willRetry: false });
		expect(host.getSessionPresentation(sessionPath).throughputTokensPerSecond).toBeUndefined();
		internals._emit({ type: "agent_settled" });
		expect(host.getRuntimeStreamSnapshot(runtime).liveEvents).toEqual([]);
		await host.disposeAll();
		vi.useRealTimers();
	});

	it("flushes every live preview when another runtime publishes an immediate lifecycle update", async () => {
		vi.useFakeTimers();
		const tempDir = join(tmpdir(), `bone-session-host-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(tempDir);
		mkdirSync(tempDir, { recursive: true });
		const firstSession = createPersistedSession(tempDir, "first session");
		const secondSession = createPersistedSession(tempDir, "second session");
		const firstPath = firstSession.getSessionFile();
		const secondPath = secondSession.getSessionFile();
		if (!firstPath || !secondPath) throw new Error("expected persisted session files");
		const factory: CreateAgentSessionRuntimeFactory = async ({
			cwd,
			agentDir,
			sessionManager,
			sessionStartEvent,
		}) => {
			const services = await createAgentSessionServices({
				cwd,
				agentDir,
				resourceLoaderOptions: { noExtensions: true, noSkills: true, noPromptTemplates: true, noThemes: true },
			});
			return {
				...(await createAgentSessionFromServices({
					services,
					sessionManager,
					sessionStartEvent,
					noTools: "all",
				})),
				services,
				diagnostics: services.diagnostics,
			};
		};
		const firstRuntime = await createAgentSessionRuntime(factory, {
			cwd: tempDir,
			agentDir: tempDir,
			sessionManager: SessionManager.open(firstPath),
		});
		const host = new InteractiveSessionHost(firstRuntime, factory);
		const firstInternals = firstRuntime.session as unknown as SessionInternals;
		firstInternals._isAgentRunActive = true;
		await host.activate(secondPath);

		const partial = assistantMsg("background partial");
		firstInternals._emit({ type: "message_start", message: assistantMsg("") });
		firstInternals._emit({
			type: "message_update",
			message: partial,
			assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "background partial", partial },
		});
		expect(host.getSessionPresentation(firstPath).livePreview).not.toBe("background partial");

		const foregroundInternals = host.current.session as unknown as SessionInternals;
		foregroundInternals._emit({ type: "agent_start" });
		expect(host.getSessionPresentation(firstPath).livePreview).toBe("background partial");

		await host.disposeAll();
		vi.useRealTimers();
	});

	it("compacts cumulative message and tool updates without dropping replay boundaries", async () => {
		const tempDir = join(tmpdir(), `bone-session-host-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(tempDir);
		mkdirSync(tempDir, { recursive: true });
		const sessionManager = createPersistedSession(tempDir, "compacted stream");
		const sessionPath = sessionManager.getSessionFile();
		if (!sessionPath) throw new Error("expected persisted session file");
		const factory: CreateAgentSessionRuntimeFactory = async ({ cwd, agentDir, sessionManager: manager }) => {
			const services = await createAgentSessionServices({
				cwd,
				agentDir,
				resourceLoaderOptions: { noExtensions: true, noSkills: true, noPromptTemplates: true, noThemes: true },
			});
			return {
				...(await createAgentSessionFromServices({ services, sessionManager: manager, noTools: "all" })),
				services,
				diagnostics: services.diagnostics,
			};
		};
		const runtime = await createAgentSessionRuntime(factory, {
			cwd: tempDir,
			agentDir: tempDir,
			sessionManager: SessionManager.open(sessionPath),
		});
		const host = new InteractiveSessionHost(runtime, factory);
		const internals = runtime.session as unknown as SessionInternals;
		const partial = assistantMsg("");
		const textPart = partial.content[0];
		const subscriberRevisions: number[] = [];
		host.subscribeRuntime(runtime, (envelope) => subscriberRevisions.push(envelope.revision));

		internals._emit({ type: "agent_start" });
		internals._emit({ type: "message_start", message: partial });
		for (let index = 0; index < 5_000; index++) {
			textPart.text += "x";
			internals._emit({
				type: "message_update",
				message: partial,
				assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: "x", partial },
			});
		}
		internals._emit({ type: "queue_update", steering: [], followUp: ["keep this boundary"] });

		let snapshot = host.getRuntimeStreamSnapshot(runtime);
		expect(snapshot.revision).toBe(5_003);
		expect(snapshot.liveEvents.map((event) => event.type)).toEqual([
			"agent_start",
			"message_start",
			"message_update",
			"queue_update",
		]);
		expect(snapshot.liveEventEnvelopes.map((envelope) => envelope.revision)).toEqual([1, 2, 5_002, 5_003]);
		const replayedUpdate = snapshot.liveEvents.find((event) => event.type === "message_update");
		expect(replayedUpdate?.type === "message_update" ? replayedUpdate.message.content[0] : undefined).toMatchObject({
			type: "text",
			text: "x".repeat(5_000),
		});
		if (replayedUpdate?.type !== "message_update") throw new Error("expected replayed message update");
		expect(replayedUpdate.message.content).toBe(replayedUpdate.assistantMessageEvent.partial.content);
		expect(subscriberRevisions).toHaveLength(5_003);
		expect(subscriberRevisions.at(-1)).toBe(5_003);

		internals._emit({ type: "message_end", message: partial });
		snapshot = host.getRuntimeStreamSnapshot(runtime);
		expect(snapshot.liveEvents.map((event) => event.type)).toEqual([
			"agent_start",
			"message_start",
			"queue_update",
			"message_end",
		]);
		expect(snapshot.liveEventEnvelopes.map((envelope) => envelope.revision)).toEqual([1, 2, 5_003, 5_004]);

		internals._emit({ type: "tool_execution_start", toolCallId: "call-1", toolName: "read", args: {} });
		for (let index = 0; index < 5_000; index++) {
			internals._emit({
				type: "tool_execution_update",
				toolCallId: "call-1",
				toolName: "read",
				args: {},
				partialResult: { content: [{ type: "text", text: `chunk ${index}` }] },
			});
		}
		snapshot = host.getRuntimeStreamSnapshot(runtime);
		expect(snapshot.liveEvents.filter((event) => event.type === "tool_execution_update")).toHaveLength(1);
		expect(snapshot.liveEventEnvelopes.at(-1)?.revision).toBe(10_005);

		internals._emit({
			type: "tool_execution_end",
			toolCallId: "call-1",
			toolName: "read",
			result: { content: [{ type: "text", text: "done" }] },
			isError: false,
		});
		snapshot = host.getRuntimeStreamSnapshot(runtime);
		expect(snapshot.liveEvents.filter((event) => event.type === "tool_execution_update")).toEqual([]);
		expect(snapshot.liveEvents.slice(-2).map((event) => event.type)).toEqual([
			"tool_execution_start",
			"tool_execution_end",
		]);
		expect(snapshot.liveEventEnvelopes.slice(-2).map((envelope) => envelope.revision)).toEqual([5_005, 10_006]);

		await internals._emitPersistedEntries([messageEntry(partial, "assistant-final")]);
		snapshot = host.getRuntimeStreamSnapshot(runtime);
		expect(snapshot.liveEvents.map((event) => event.type)).toEqual([
			"agent_start",
			"queue_update",
			"tool_execution_start",
			"tool_execution_end",
		]);
		expect(snapshot.liveEventEnvelopes.map((envelope) => envelope.revision)).toEqual([1, 5_003, 5_005, 10_006]);

		await internals._emitPersistedEntries([
			messageEntry(
				{
					role: "toolResult",
					toolCallId: "call-1",
					toolName: "read",
					content: [{ type: "text", text: "done" }],
					isError: false,
					timestamp: Date.now(),
				},
				"tool-final",
			),
		]);
		expect(host.getRuntimeStreamSnapshot(runtime).liveEvents.map((event) => event.type)).toEqual([
			"agent_start",
			"queue_update",
		]);

		await host.disposeAll();
	});

	it("switches away from the foreground conversation before soft-deleting it", async () => {
		const tempDir = join(tmpdir(), `bone-session-host-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(tempDir);
		mkdirSync(tempDir, { recursive: true });

		const firstSession = createPersistedSession(tempDir, "first session");
		const secondSession = createPersistedSession(tempDir, "second session");
		const firstPath = firstSession.getSessionFile();
		const secondPath = secondSession.getSessionFile();
		if (!firstPath || !secondPath) throw new Error("expected persisted session files");

		const factory: CreateAgentSessionRuntimeFactory = async ({
			cwd,
			agentDir,
			sessionManager,
			sessionStartEvent,
		}) => {
			const services = await createAgentSessionServices({
				cwd,
				agentDir,
				resourceLoaderOptions: {
					noExtensions: true,
					noSkills: true,
					noPromptTemplates: true,
					noThemes: true,
				},
			});
			return {
				...(await createAgentSessionFromServices({
					services,
					sessionManager,
					sessionStartEvent,
					noTools: "all",
				})),
				services,
				diagnostics: services.diagnostics,
			};
		};

		const initialRuntime = await createAgentSessionRuntime(factory, {
			cwd: tempDir,
			agentDir: tempDir,
			sessionManager: SessionManager.open(firstPath),
		});
		const host = new InteractiveSessionHost(initialRuntime, factory);
		rememberLastActiveConversation(
			tempDir,
			initialRuntime.session.sessionManager.getSessionDir(),
			firstPath,
			tempDir,
		);

		const result = await host.deleteSession(firstPath, secondPath);

		expect(["system-trash", "bone-trash"]).toContain(result.method);
		expect(existsSync(firstPath)).toBe(false);
		expect(host.current.session.sessionFile).toBe(secondPath);
		expect(
			getLastActiveConversation(tempDir, initialRuntime.session.sessionManager.getSessionDir(), tempDir),
		).toBeUndefined();
		await host.disposeAll();
	});
});
