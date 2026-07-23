import { existsSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
import { SessionManager } from "../src/core/session-manager.ts";
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
