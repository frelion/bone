import { existsSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
	type CreateAgentSessionRuntimeFactory,
	createAgentSessionFromServices,
	createAgentSessionRuntime,
	createAgentSessionServices,
} from "../src/core/agent-session-runtime.ts";
import { InteractiveSessionHost } from "../src/core/interactive-session-host.ts";
import { SessionManager } from "../src/core/session-manager.ts";
import { assistantMsg, userMsg } from "./utilities.ts";

type SessionInternals = {
	_emitAgentSettled(): Promise<void>;
	_isAgentRunActive: boolean;
};

function createPersistedSession(cwd: string, text: string): SessionManager {
	const sessionManager = SessionManager.create(cwd);
	sessionManager.appendMessage(userMsg(text));
	sessionManager.appendMessage(assistantMsg(`${text} response`));
	return sessionManager;
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
});
