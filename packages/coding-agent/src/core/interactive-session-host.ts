import { resolvePath } from "../utils/paths.ts";
import type { AgentSessionEvent } from "./agent-session.ts";
import {
	type AgentSessionRuntime,
	type CreateAgentSessionRuntimeFactory,
	createAgentSessionRuntime,
} from "./agent-session-runtime.ts";
import { type SessionInfo, SessionManager } from "./session-manager.ts";

export type InteractiveSessionState = "foreground" | "background-running" | "background-waiting" | "cold";

export interface InteractiveSessionSummary extends SessionInfo {
	state: InteractiveSessionState;
}

interface RuntimeSlot {
	runtime: AgentSessionRuntime;
	state: Exclude<InteractiveSessionState, "cold">;
	unsubscribe: () => void;
}

function extractText(content: unknown): string {
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	return content
		.filter(
			(part): part is { type: "text"; text: string } =>
				typeof part === "object" &&
				part !== null &&
				"type" in part &&
				part.type === "text" &&
				"text" in part &&
				typeof part.text === "string",
		)
		.map((part) => part.text)
		.join(" ");
}

export interface InteractiveSessionHostHooks {
	/** Detach session-bound UI before the current foreground runtime is parked. */
	beforeForegroundChange?: (runtime: AgentSessionRuntime) => Promise<void>;
	/** Bind and render the newly selected foreground runtime. */
	foregroundChanged?: (runtime: AgentSessionRuntime) => Promise<void>;
	/** Release UI retained for a runtime that is no longer live. */
	runtimeDisposed?: (runtime: AgentSessionRuntime) => void;
	/** Refresh any session status UI after a lifecycle transition. */
	stateChanged?: () => void;
}

/**
 * Owns the short-lived runtime instances used by the interactive TUI.
 *
 * A selected session is always live. A session that is switched away while it
 * is still working stays live only until it emits `agent_settled`; then its
 * runtime is disposed and the JSONL session remains as the source of truth.
 */
export class InteractiveSessionHost {
	private foreground: RuntimeSlot;
	private background = new Map<string, RuntimeSlot>();
	private hooks: InteractiveSessionHostHooks = {};
	private transition: Promise<void> = Promise.resolve();
	private readonly createRuntime: CreateAgentSessionRuntimeFactory;

	constructor(initialRuntime: AgentSessionRuntime, createRuntime: CreateAgentSessionRuntimeFactory) {
		this.createRuntime = createRuntime;
		this.foreground = this.createSlot(initialRuntime, "foreground");
	}

	get current(): AgentSessionRuntime {
		return this.foreground.runtime;
	}

	setHooks(hooks: InteractiveSessionHostHooks): void {
		this.hooks = hooks;
	}

	async activate(sessionPath: string, options?: { cwdOverride?: string }): Promise<void> {
		return this.enqueue(async () => {
			const targetPath = this.normalizeSessionPath(sessionPath);
			const currentPath = this.getSessionPath(this.foreground.runtime);
			if (targetPath === currentPath) return;

			await this.hooks.beforeForegroundChange?.(this.foreground.runtime);
			await this.parkForeground();

			const existing = this.background.get(targetPath);
			if (existing) {
				this.background.delete(targetPath);
				existing.state = "foreground";
				this.foreground = existing;
			} else {
				const runtime = await this.openRuntime(targetPath, currentPath, options?.cwdOverride);
				this.foreground = this.createSlot(runtime, "foreground");
			}

			await this.hooks.foregroundChanged?.(this.foreground.runtime);
			this.hooks.stateChanged?.();
		});
	}

	async createNew(): Promise<void> {
		return this.enqueue(async () => {
			const currentRuntime = this.foreground.runtime;
			const currentManager = currentRuntime.session.sessionManager;
			const previousSessionFile = this.getSessionPath(currentRuntime);
			const sessionManager = currentManager.isPersisted()
				? SessionManager.create(currentManager.getCwd(), currentManager.getSessionDir())
				: SessionManager.inMemory(currentManager.getCwd());

			await this.hooks.beforeForegroundChange?.(currentRuntime);
			await this.parkForeground();

			const runtime = await createAgentSessionRuntime(this.createRuntime, {
				cwd: sessionManager.getCwd(),
				agentDir: currentRuntime.services.agentDir,
				sessionManager,
				sessionStartEvent: {
					type: "session_start",
					reason: "new",
					previousSessionFile,
				},
			});
			this.foreground = this.createSlot(runtime, "foreground");
			await this.hooks.foregroundChanged?.(runtime);
			this.hooks.stateChanged?.();
		});
	}

	async list(): Promise<InteractiveSessionSummary[]> {
		const current = this.current.session.sessionManager;
		const sessions = await SessionManager.list(current.getCwd(), current.getSessionDir());
		const currentPath = this.getSessionPath(this.current);

		// A first turn is held in memory until an assistant message is persisted.
		// Keep live runtimes visible so switching away cannot make that turn unreachable.
		for (const slot of [this.foreground, ...this.background.values()]) {
			const sessionPath = this.getSessionPath(slot.runtime);
			if (!sessionPath || sessions.some((session) => this.normalizeSessionPath(session.path) === sessionPath)) {
				continue;
			}
			sessions.unshift(this.createRuntimeSessionInfo(slot.runtime, sessionPath));
		}
		return sessions.map((session) => {
			const path = this.normalizeSessionPath(session.path);
			const background = this.background.get(path);
			return {
				...session,
				state: path === currentPath ? "foreground" : (background?.state ?? "cold"),
			};
		});
	}

	async disposeAll(): Promise<void> {
		return this.enqueue(async () => {
			const slots = [this.foreground, ...this.background.values()];
			this.background.clear();
			for (const slot of slots) {
				slot.unsubscribe();
				try {
					await slot.runtime.dispose();
				} finally {
					this.hooks.runtimeDisposed?.(slot.runtime);
				}
			}
		});
	}

	/** Wait until currently queued lifecycle transitions have completed. */
	async waitForTransitions(): Promise<void> {
		await this.transition;
	}

	/**
	 * Serialize a configuration refresh with switching and background disposal.
	 * The callback runs for the foreground and every still-streaming background
	 * runtime, so a settings save cannot race a session lifecycle transition.
	 */
	async refreshLiveRuntimes(refresh: (runtime: AgentSessionRuntime) => Promise<void>): Promise<void> {
		return this.enqueue(async () => {
			for (const slot of [this.foreground, ...this.background.values()]) {
				await refresh(slot.runtime);
			}
		});
	}

	private createSlot(runtime: AgentSessionRuntime, state: Exclude<InteractiveSessionState, "cold">): RuntimeSlot {
		const slot: RuntimeSlot = {
			runtime,
			state,
			unsubscribe: () => {},
		};
		const subscribe = () =>
			runtime.session.subscribe((event) => {
				this.handleRuntimeEvent(slot, event);
			});
		slot.unsubscribe = subscribe();
		runtime.setRebindSession(async () => {
			slot.unsubscribe();
			slot.unsubscribe = subscribe();
			if (this.foreground !== slot) return;
			await this.hooks.foregroundChanged?.(runtime);
			this.hooks.stateChanged?.();
		});
		runtime.setBeforeSessionInvalidate(() => {
			if (this.foreground !== slot) return;
			void this.hooks.beforeForegroundChange?.(runtime);
		});
		return slot;
	}

	private handleRuntimeEvent(slot: RuntimeSlot, event: AgentSessionEvent): void {
		if (slot.state === "background-running" && event.type === "agent_settled") {
			void this.enqueue(async () => {
				if (slot.state !== "background-running") return;
				await this.suspend(slot);
				this.hooks.stateChanged?.();
			});
		}
	}

	private async parkForeground(): Promise<void> {
		const slot = this.foreground;
		const sessionPath = this.getSessionPath(slot.runtime);
		if (!sessionPath) {
			throw new Error("Cannot switch sessions when the current session is not persisted");
		}

		if (!this.hasOngoingWork(slot.runtime)) {
			await this.suspend(slot);
			return;
		}

		slot.state = "background-running";
		this.background.set(sessionPath, slot);
	}

	private hasOngoingWork(runtime: AgentSessionRuntime): boolean {
		const session = runtime.session;
		return session.isStreaming || session.isCompacting || session.isBashRunning;
	}

	private async suspend(slot: RuntimeSlot): Promise<void> {
		const sessionPath = this.getSessionPath(slot.runtime);
		if (sessionPath) this.background.delete(sessionPath);
		slot.unsubscribe();
		try {
			await slot.runtime.dispose();
		} finally {
			this.hooks.runtimeDisposed?.(slot.runtime);
		}
	}

	private async openRuntime(
		sessionPath: string,
		previousSessionFile?: string,
		cwdOverride?: string,
	): Promise<AgentSessionRuntime> {
		const sessionManager = SessionManager.open(sessionPath, undefined, cwdOverride);
		return createAgentSessionRuntime(this.createRuntime, {
			cwd: sessionManager.getCwd(),
			agentDir: this.current.services.agentDir,
			sessionManager,
			sessionStartEvent: {
				type: "session_start",
				reason: "resume",
				previousSessionFile,
			},
		});
	}

	private getSessionPath(runtime: AgentSessionRuntime): string | undefined {
		const sessionFile = runtime.session.sessionFile;
		return sessionFile ? this.normalizeSessionPath(sessionFile) : undefined;
	}

	private createRuntimeSessionInfo(runtime: AgentSessionRuntime, path: string): SessionInfo {
		const sessionManager = runtime.session.sessionManager;
		const header = sessionManager.getHeader();
		const messages = sessionManager
			.getEntries()
			.filter((entry) => entry.type === "message")
			.map((entry) => entry.message)
			.filter((message) => message.role === "user" || message.role === "assistant");
		const firstUserMessage = messages.find((message) => message.role === "user");
		const firstMessage = firstUserMessage
			? extractText(firstUserMessage.content) || "(no messages)"
			: "(no messages)";
		const lastActivityMessage = messages.at(-1);
		const lastMessage = [...messages].reverse().find((message) => extractText(message.content).trim().length > 0);
		const lastMessageText = lastMessage ? extractText(lastMessage.content) : "";
		const lastMessageRole =
			lastMessage?.role === "user" || lastMessage?.role === "assistant" ? lastMessage.role : undefined;
		const created = header ? new Date(header.timestamp) : new Date();
		const lastMessageTimestamp =
			lastActivityMessage && "timestamp" in lastActivityMessage ? lastActivityMessage.timestamp : undefined;
		const modified = typeof lastMessageTimestamp === "number" ? new Date(lastMessageTimestamp) : created;

		return {
			path,
			id: sessionManager.getSessionId(),
			cwd: sessionManager.getCwd(),
			name: sessionManager.getSessionName(),
			parentSessionPath: header?.parentSession,
			created,
			modified,
			messageCount: messages.length,
			firstMessage,
			allMessagesText: firstMessage,
			lastMessage: lastMessageText || undefined,
			lastMessageRole: lastMessageText ? lastMessageRole : undefined,
		};
	}

	private normalizeSessionPath(sessionPath: string): string {
		return resolvePath(sessionPath);
	}

	private enqueue(operation: () => Promise<void>): Promise<void> {
		const next = this.transition.then(operation, operation);
		this.transition = next.catch(() => {});
		return next;
	}
}
