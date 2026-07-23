import { existsSync } from "node:fs";
import { extname, relative, sep } from "node:path";
import type { AgentMessage } from "@frelion/bone-agent-core";
import { resolvePath } from "../utils/paths.ts";
import type { AgentSessionEvent, PromptOptions } from "./agent-session.ts";
import {
	type AgentSessionRuntime,
	type CreateAgentSessionRuntimeFactory,
	createAgentSessionRuntime,
} from "./agent-session-runtime.ts";
import { forgetLastActiveConversation } from "./conversation-state.ts";
import { type SessionEntry, type SessionInfo, SessionManager } from "./session-manager.ts";
import { type SessionTrashMethod, softDeleteSessionFile } from "./session-trash.ts";

export type InteractiveSessionState = "foreground" | "background-running" | "background-waiting" | "cold";

export interface InteractiveSessionSummary extends SessionInfo {
	state: InteractiveSessionState;
	livePreview?: string;
	throughputTokensPerSecond?: number;
}

export interface InteractiveSessionPresentation {
	state: InteractiveSessionState;
	livePreview?: string;
	throughputTokensPerSecond?: number;
	messageCount: number;
	modified: Date;
}

export type InteractiveSessionDeletionResult = { method: SessionTrashMethod } | { method: "discarded" };

export interface RuntimeEventEnvelope {
	runtime: AgentSessionRuntime;
	revision: number;
	generationId: string | undefined;
	event: AgentSessionEvent;
}

export interface RuntimeStreamSnapshot {
	revision: number;
	generationId: string | undefined;
	liveEvents: readonly AgentSessionEvent[];
	liveEventEnvelopes: readonly RuntimeEventEnvelope[];
}

interface RuntimeSlot {
	runtime: AgentSessionRuntime;
	state: Exclude<InteractiveSessionState, "cold">;
	acceptingPrompts: boolean;
	pendingPrompts: number;
	promptTail: Promise<void>;
	unsubscribe: () => void;
	unsubscribePersistedEntries: () => void;
	unsubscribeRunCompleted: () => void;
	livePreview: string | undefined;
	messageCount: number;
	modified: Date;
	generationStartedAt: number | undefined;
	streamedCharacters: number;
	revision: number;
	generationId: string | undefined;
	liveEvents: AgentSessionEvent[];
	liveEventRevisions: number[];
	streamListeners: Set<(envelope: RuntimeEventEnvelope) => void>;
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

function normalizeLivePreview(text: string): string {
	return text
		.replace(/[\r\n\t]+/g, " ")
		.replace(/\s+/g, " ")
		.trim();
}

/** Agent events contain mutable partial messages; replay must capture their value at emission time. */
function cloneAgentSessionEvent(event: AgentSessionEvent): AgentSessionEvent {
	if (typeof structuredClone === "function") {
		try {
			return structuredClone(event);
		} catch {
			// Extension-defined event payloads can contain values structuredClone rejects.
		}
	}
	const clone = (value: unknown): unknown => {
		if (value === null || typeof value !== "object") return value;
		if (value instanceof Date) return new Date(value);
		if (Array.isArray(value)) return value.map(clone);
		return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, clone(item)]));
	};
	return clone(event) as AgentSessionEvent;
}

function messageReplayKey(message: AgentMessage): string {
	const toolCallId = "toolCallId" in message && typeof message.toolCallId === "string" ? message.toolCallId : "";
	return `${message.role}:${message.timestamp}:${toolCallId}`;
}

function messageContentSignature(message: AgentMessage): string {
	try {
		return JSON.stringify("content" in message ? message.content : message);
	} catch {
		return String("content" in message ? message.content : message);
	}
}

export interface InteractiveSessionHostHooks {
	/** Detach session-bound UI before the current foreground runtime is parked. */
	beforeForegroundChange?: (runtime: AgentSessionRuntime) => Promise<void>;
	/** Bind and render the newly selected foreground runtime. */
	foregroundChanged?: (runtime: AgentSessionRuntime) => Promise<void>;
	/** Release UI retained for a runtime that is no longer live. */
	runtimeDisposed?: (runtime: AgentSessionRuntime) => void;
	/** Refresh any session status UI after a lifecycle transition. */
	stateChanged?: (structureChanged: boolean) => void;
	/** Materialize entries that have successfully reached a JSONL session file. */
	persistedEntries?: (runtime: AgentSessionRuntime, entries: readonly SessionEntry[]) => Promise<void>;
	/** Materialize the final, user-visible response for a completed agent run. */
	runCompleted?: (runtime: AgentSessionRuntime, messages: readonly AgentMessage[]) => Promise<void>;
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
	private readonly presentationByPath = new Map<string, Omit<InteractiveSessionPresentation, "state">>();
	private presentationRefreshTimer: ReturnType<typeof setTimeout> | undefined;

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
		return this.enqueue(async () => await this.activateWithinTransition(sessionPath, options));
	}

	async createNew(): Promise<void> {
		return this.enqueue(async () => await this.createNewWithinTransition());
	}

	/** Route composer input to the runtime that was foreground when it was submitted. */
	async prompt(runtime: AgentSessionRuntime, text: string, options?: PromptOptions): Promise<void> {
		const slot = this.findRuntimeSlot(runtime);
		if (!slot || !slot.acceptingPrompts) throw new Error("Conversation is no longer active");
		const submittedSession = runtime.session;

		slot.pendingPrompts++;
		const prompt = slot.promptTail
			.catch(() => {})
			.then(async () => {
				if (!slot.acceptingPrompts || runtime.session !== submittedSession) {
					throw new Error("Conversation runtime was replaced");
				}
				await submittedSession.prompt(text, options);
			});
		slot.promptTail = prompt;
		try {
			await prompt;
		} finally {
			slot.pendingPrompts--;
			this.scheduleBackgroundSuspend(slot);
		}
	}

	/**
	 * Stop, dispose, and soft-delete a conversation without allowing its runtime
	 * lifecycle to race a JSONL move. The replacement path is selected by Side.
	 */
	async deleteSession(
		sessionPath: string,
		replacementSessionPath?: string,
	): Promise<InteractiveSessionDeletionResult> {
		return this.enqueue(async () => {
			const targetPath = this.normalizeSessionPath(sessionPath);
			const currentRuntime = this.foreground.runtime;
			const currentManager = currentRuntime.session.sessionManager;
			const sessionDir = this.normalizeSessionPath(currentManager.getSessionDir());
			if (!this.isManagedSessionPath(targetPath, sessionDir)) {
				throw new Error("Conversation path is outside the managed session directory");
			}

			const sessions = await SessionManager.list(currentManager.getCwd(), currentManager.getSessionDir());
			const targetSlot = this.findLiveSlot(targetPath);
			if (!targetSlot && !sessions.some((session) => this.normalizeSessionPath(session.path) === targetPath)) {
				throw new Error("Conversation no longer exists");
			}

			const agentDir = currentRuntime.services.agentDir;
			if (targetSlot === this.foreground) {
				targetSlot.acceptingPrompts = false;
				if (this.hasOngoingWork(targetSlot)) {
					await targetSlot.runtime.session.abort();
				}
				await targetSlot.promptTail.catch(() => {});

				const replacementPath = replacementSessionPath && this.normalizeSessionPath(replacementSessionPath);
				const replacementExists =
					replacementPath !== targetPath &&
					sessions.some((session) => this.normalizeSessionPath(session.path) === replacementPath);
				if (replacementExists && replacementPath) {
					await this.activateWithinTransition(replacementPath);
				} else {
					await this.createNewWithinTransition();
				}
			} else if (targetSlot) {
				// Suppress the queued background-settled cleanup: this deletion owns the
				// slot until it has been fully stopped and disposed exactly once.
				targetSlot.state = "background-waiting";
				targetSlot.acceptingPrompts = false;
				if (this.hasOngoingWork(targetSlot)) {
					await targetSlot.runtime.session.abort();
				}
				await targetSlot.promptTail.catch(() => {});
				await this.suspend(targetSlot);
			}

			if (!existsSync(targetPath)) {
				forgetLastActiveConversation(targetPath, agentDir);
				this.hooks.stateChanged?.(true);
				return { method: "discarded" };
			}

			const result = await softDeleteSessionFile(targetPath, agentDir);
			if (!result.ok) throw new Error(result.error);
			forgetLastActiveConversation(targetPath, agentDir);
			this.hooks.stateChanged?.(true);
			return { method: result.method };
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
		return sessions.map((session) => this.decorateSessionInfo(session, currentPath));
	}

	async listPage(
		offset: number,
		limit: number,
	): Promise<{ sessions: InteractiveSessionSummary[]; total: number; hasMore: boolean; nextOffset: number }> {
		const current = this.current.session.sessionManager;
		const page = await SessionManager.listPage(current.getCwd(), current.getSessionDir(), offset, limit);
		const currentPath = this.getSessionPath(this.current);
		const sessions = page.sessions.map((session) => this.decorateSessionInfo(session, currentPath));
		if (offset === 0) {
			for (const slot of [this.foreground, ...this.background.values()]) {
				const sessionPath = this.getSessionPath(slot.runtime);
				if (!sessionPath || sessions.some((session) => this.normalizeSessionPath(session.path) === sessionPath))
					continue;
				sessions.unshift(
					this.decorateSessionInfo(this.createRuntimeSessionInfo(slot.runtime, sessionPath), currentPath),
				);
			}
		}
		return { ...page, sessions, nextOffset: Math.min(page.total, Math.max(0, offset) + Math.max(1, limit)) };
	}

	private decorateSessionInfo(session: SessionInfo, currentPath: string | undefined): InteractiveSessionSummary {
		const path = this.normalizeSessionPath(session.path);
		const slot = this.findLiveSlot(path);
		const presentation = slot
			? this.presentationForSlot(slot)
			: (this.presentationByPath.get(path) ?? {
					livePreview: session.lastMessage,
					throughputTokensPerSecond: undefined,
					messageCount: session.messageCount,
					modified: session.modified,
				});
		this.presentationByPath.set(path, presentation);
		return {
			...session,
			...presentation,
			path,
			state: path === currentPath ? "foreground" : (this.background.get(path)?.state ?? "cold"),
		};
	}

	getSessionState(sessionPath: string): InteractiveSessionState {
		const path = this.normalizeSessionPath(sessionPath);
		if (path === this.getSessionPath(this.current)) return "foreground";
		return this.background.get(path)?.state ?? "cold";
	}

	getSessionPresentation(sessionPath: string): InteractiveSessionPresentation {
		const path = this.normalizeSessionPath(sessionPath);
		const slot = this.findLiveSlot(path);
		const recent = slot ? this.presentationForSlot(slot) : this.presentationByPath.get(path);
		return {
			state: this.getSessionState(path),
			...recent,
			messageCount: recent?.messageCount ?? 0,
			modified: recent?.modified ?? new Date(0),
		};
	}

	/** Read the non-durable stream currently held by a live runtime. */
	getRuntimeStreamSnapshot(runtime: AgentSessionRuntime): RuntimeStreamSnapshot {
		const slot = this.findRuntimeSlot(runtime);
		if (!slot) return { revision: 0, generationId: undefined, liveEvents: [], liveEventEnvelopes: [] };
		const liveEventEnvelopes = slot.liveEvents.map((event, index) => ({
			runtime: slot.runtime,
			revision: slot.liveEventRevisions[index] ?? slot.revision,
			generationId: slot.generationId,
			event: cloneAgentSessionEvent(event),
		}));
		return {
			revision: slot.revision,
			generationId: slot.generationId,
			liveEvents: liveEventEnvelopes.map(({ event }) => event),
			liveEventEnvelopes,
		};
	}

	/** Subscribe to a runtime's ordered event stream. This remains attached while the runtime is backgrounded. */
	subscribeRuntime(runtime: AgentSessionRuntime, listener: (envelope: RuntimeEventEnvelope) => void): () => void {
		const slot = this.findRuntimeSlot(runtime);
		if (!slot) return () => {};
		slot.streamListeners.add(listener);
		return () => slot.streamListeners.delete(listener);
	}

	async getSessionSummaries(paths: readonly string[]): Promise<InteractiveSessionSummary[]> {
		const currentPath = this.getSessionPath(this.current);
		const summaries = await Promise.all(paths.map(async (path) => await SessionManager.getInfo(path)));
		return summaries
			.filter((summary): summary is SessionInfo => summary !== null)
			.map((summary) => this.decorateSessionInfo(summary, currentPath));
	}

	async disposeAll(): Promise<void> {
		return this.enqueue(async () => {
			if (this.presentationRefreshTimer) clearTimeout(this.presentationRefreshTimer);
			this.presentationRefreshTimer = undefined;
			const slots = [this.foreground, ...this.background.values()];
			this.background.clear();
			for (const slot of slots) {
				slot.unsubscribe();
				slot.unsubscribePersistedEntries();
				slot.unsubscribeRunCompleted();
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

	/** Run a foreground UI rebind inside the same queue as session activation. */
	async refreshForeground(task: (runtime: AgentSessionRuntime) => Promise<void>): Promise<void> {
		return this.enqueue(async () => await task(this.foreground.runtime));
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
		const messages = runtime.session.sessionManager
			.getEntries()
			.filter((entry) => entry.type === "message")
			.map((entry) => entry.message)
			.filter((message) => message.role === "user" || message.role === "assistant");
		const latestMessage = [...messages].reverse().find((message) => extractText(message.content).trim().length > 0);
		const latestTimestamp = messages.at(-1)?.timestamp;
		const headerTimestamp = runtime.session.sessionManager.getHeader()?.timestamp;
		const slot: RuntimeSlot = {
			runtime,
			state,
			acceptingPrompts: true,
			pendingPrompts: 0,
			promptTail: Promise.resolve(),
			unsubscribe: () => {},
			unsubscribePersistedEntries: () => {},
			unsubscribeRunCompleted: () => {},
			livePreview: latestMessage ? normalizeLivePreview(extractText(latestMessage.content)) : undefined,
			messageCount: messages.length,
			modified:
				typeof latestTimestamp === "number"
					? new Date(latestTimestamp)
					: headerTimestamp
						? new Date(headerTimestamp)
						: new Date(),
			generationStartedAt: runtime.session.isStreaming ? Date.now() : undefined,
			streamedCharacters: 0,
			revision: 0,
			generationId: undefined,
			liveEvents: [],
			liveEventRevisions: [],
			streamListeners: new Set(),
		};
		const subscribe = () =>
			runtime.session.subscribe((event) => {
				this.handleRuntimeEvent(slot, event);
			});
		slot.unsubscribe = subscribe();
		slot.unsubscribePersistedEntries = runtime.session.subscribePersistedEntries(async (entries) => {
			this.reconcilePersistedEntries(slot, entries);
			await this.hooks.persistedEntries?.(runtime, entries);
		});
		slot.unsubscribeRunCompleted = runtime.session.subscribeRunCompleted(async (messages) => {
			await this.hooks.runCompleted?.(runtime, messages);
		});
		runtime.setRebindSession(async () => {
			slot.unsubscribe();
			slot.unsubscribe = subscribe();
			if (this.foreground !== slot) return;
			await this.hooks.foregroundChanged?.(runtime);
			this.hooks.stateChanged?.(true);
		});
		runtime.setBeforeSessionInvalidate(() => {
			if (this.foreground !== slot) return;
			void this.hooks.beforeForegroundChange?.(runtime);
		});
		return slot;
	}

	private reconcilePersistedEntries(slot: RuntimeSlot, entries: readonly SessionEntry[]): void {
		for (const entry of entries) {
			if (entry.type !== "message") continue;
			const persistedKey = messageReplayKey(entry.message);
			const completedToolCallId =
				entry.message.role === "toolResult" && "toolCallId" in entry.message ? entry.message.toolCallId : undefined;
			const matchingEndIndex = slot.liveEvents.findIndex(
				(event) =>
					event.type === "message_end" &&
					messageReplayKey(event.message) === persistedKey &&
					messageContentSignature(event.message) === messageContentSignature(entry.message),
			);
			const removeMessageRange = matchingEndIndex >= 0;
			let matchingStartIndex = -1;
			if (removeMessageRange) {
				for (let index = matchingEndIndex - 1; index >= 0; index--) {
					const event = slot.liveEvents[index];
					if (event.type === "message_start" && messageReplayKey(event.message) === persistedKey) {
						matchingStartIndex = index;
						break;
					}
				}
			}
			const rangeStart = matchingStartIndex >= 0 ? matchingStartIndex : matchingEndIndex;
			const retainedEvents: AgentSessionEvent[] = [];
			const retainedRevisions: number[] = [];
			for (const [index, event] of slot.liveEvents.entries()) {
				let keep = true;
				if (removeMessageRange && index >= rangeStart && index <= matchingEndIndex) {
					keep = false;
				}
				if (keep && (event.type === "message_start" || event.type === "message_end")) {
					if (!removeMessageRange && messageReplayKey(event.message) === persistedKey) keep = false;
				}
				if (keep && event.type === "message_update") {
					if (!removeMessageRange && messageReplayKey(event.message) === persistedKey) keep = false;
				}
				if (
					keep &&
					completedToolCallId &&
					(event.type === "tool_execution_start" ||
						event.type === "tool_execution_update" ||
						event.type === "tool_execution_end")
				) {
					if (event.toolCallId === completedToolCallId) keep = false;
				}
				if (keep) {
					retainedEvents.push(event);
					retainedRevisions.push(slot.liveEventRevisions[index] ?? slot.revision);
				}
			}
			slot.liveEvents = retainedEvents;
			slot.liveEventRevisions = retainedRevisions;
		}
	}

	private handleRuntimeEvent(slot: RuntimeSlot, event: AgentSessionEvent): void {
		const now = Date.now();
		if (event.type === "agent_start") {
			slot.generationId = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
			slot.liveEvents = [];
			slot.liveEventRevisions = [];
		}
		slot.revision++;
		const eventSnapshot = cloneAgentSessionEvent(event);
		if (slot.generationId !== undefined) {
			slot.liveEvents.push(eventSnapshot);
			slot.liveEventRevisions.push(slot.revision);
		}
		const envelope: RuntimeEventEnvelope = {
			runtime: slot.runtime,
			revision: slot.revision,
			generationId: slot.generationId,
			event: eventSnapshot,
		};
		for (const listener of slot.streamListeners) listener(envelope);
		if (event.type === "agent_start") {
			slot.generationStartedAt = now;
			slot.streamedCharacters = 0;
			slot.modified = new Date(now);
			this.publishPresentation(slot, true);
		} else if (event.type === "message_start") {
			if (event.message.role === "user" || event.message.role === "assistant") {
				slot.messageCount = (slot.messageCount ?? 0) + 1;
				const text = normalizeLivePreview(extractText(event.message.content));
				if (text) slot.livePreview = text;
				slot.modified = new Date(now);
				this.publishPresentation(slot);
			}
		} else if (event.type === "message_update") {
			if (event.assistantMessageEvent.type === "text_delta") {
				slot.streamedCharacters += event.assistantMessageEvent.delta.length;
				const text = normalizeLivePreview(extractText(event.assistantMessageEvent.partial.content));
				if (text) slot.livePreview = text;
				slot.modified = new Date(now);
				this.publishPresentation(slot);
			}
		} else if (event.type === "message_end") {
			if (event.message.role === "user" || event.message.role === "assistant") {
				const text = normalizeLivePreview(extractText(event.message.content));
				if (text) slot.livePreview = text;
				slot.modified = new Date(now);
				this.publishPresentation(slot);
			}
		} else if (event.type === "agent_end" || event.type === "agent_settled") {
			slot.generationStartedAt = undefined;
			slot.modified = new Date(now);
			this.publishPresentation(slot, true);
		}
		if (event.type === "agent_settled") {
			slot.liveEvents = [];
			slot.liveEventRevisions = [];
			slot.generationId = undefined;
		}
		if (slot.state === "background-running" && event.type === "agent_settled") {
			this.scheduleBackgroundSuspend(slot);
		}
	}

	private presentationForSlot(slot: RuntimeSlot): Omit<InteractiveSessionPresentation, "state"> {
		const elapsedSeconds =
			slot.generationStartedAt !== undefined ? (Date.now() - slot.generationStartedAt) / 1000 : 0;
		const throughputTokensPerSecond =
			elapsedSeconds > 0.2 && slot.streamedCharacters > 0 ? slot.streamedCharacters / 4 / elapsedSeconds : undefined;
		return {
			livePreview: slot.livePreview,
			throughputTokensPerSecond,
			messageCount: slot.messageCount,
			modified: slot.modified,
		};
	}

	private publishPresentation(slot: RuntimeSlot, immediate = false): void {
		const path = this.getSessionPath(slot.runtime);
		if (path) this.presentationByPath.set(path, this.presentationForSlot(slot));
		if (immediate) {
			if (this.presentationRefreshTimer) clearTimeout(this.presentationRefreshTimer);
			this.presentationRefreshTimer = undefined;
			this.hooks.stateChanged?.(false);
			return;
		}
		if (this.presentationRefreshTimer) return;
		this.presentationRefreshTimer = setTimeout(() => {
			this.presentationRefreshTimer = undefined;
			for (const liveSlot of [this.foreground, ...this.background.values()]) {
				const livePath = this.getSessionPath(liveSlot.runtime);
				if (livePath) this.presentationByPath.set(livePath, this.presentationForSlot(liveSlot));
			}
			this.hooks.stateChanged?.(false);
		}, 250);
	}

	private async activateWithinTransition(sessionPath: string, options?: { cwdOverride?: string }): Promise<void> {
		const timingEnabled = process.env.BONE_TIMING === "1";
		const totalStartedAt = performance.now();
		const timings: Record<string, number> = {};
		const targetPath = this.normalizeSessionPath(sessionPath);
		const currentPath = this.getSessionPath(this.foreground.runtime);
		if (targetPath === currentPath) return;

		let startedAt = performance.now();
		await this.hooks.beforeForegroundChange?.(this.foreground.runtime);
		timings.uiDetach = performance.now() - startedAt;
		startedAt = performance.now();
		await this.parkForeground();
		timings.teardown = performance.now() - startedAt;

		const existing = this.background.get(targetPath);
		if (existing) {
			this.background.delete(targetPath);
			existing.state = "foreground";
			this.foreground = existing;
		} else {
			const runtime = await this.openRuntime(targetPath, currentPath, options?.cwdOverride, timings);
			this.foreground = this.createSlot(runtime, "foreground");
		}

		startedAt = performance.now();
		await this.hooks.foregroundChanged?.(this.foreground.runtime);
		timings.foregroundBind = performance.now() - startedAt;
		this.hooks.stateChanged?.(false);
		if (timingEnabled) {
			timings.total = performance.now() - totalStartedAt;
			console.error(
				`[bone switch] ${Object.entries(timings)
					.map(([label, ms]) => `${label}=${ms.toFixed(1)}ms`)
					.join(" ")}`,
			);
		}
	}

	private async createNewWithinTransition(): Promise<void> {
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
		this.hooks.stateChanged?.(true);
	}

	private async parkForeground(): Promise<void> {
		const slot = this.foreground;
		const sessionPath = this.getSessionPath(slot.runtime);
		if (!sessionPath) {
			throw new Error("Cannot switch sessions when the current session is not persisted");
		}

		if (!this.hasOngoingWork(slot)) {
			await this.suspend(slot);
			return;
		}

		slot.state = "background-running";
		this.background.set(sessionPath, slot);
	}

	private hasOngoingWork(slot: RuntimeSlot): boolean {
		const session = slot.runtime.session;
		return slot.pendingPrompts > 0 || session.isStreaming || session.isCompacting || session.isBashRunning;
	}

	private scheduleBackgroundSuspend(slot: RuntimeSlot): void {
		if (slot.state !== "background-running" || this.hasOngoingWork(slot)) return;
		void this.enqueue(async () => {
			if (slot.state !== "background-running" || this.hasOngoingWork(slot)) return;
			await this.suspend(slot);
			this.hooks.stateChanged?.(false);
		});
	}

	private findRuntimeSlot(runtime: AgentSessionRuntime): RuntimeSlot | undefined {
		if (this.foreground.runtime === runtime) return this.foreground;
		for (const slot of this.background.values()) {
			if (slot.runtime === runtime) return slot;
		}
		return undefined;
	}

	private async suspend(slot: RuntimeSlot): Promise<void> {
		const sessionPath = this.getSessionPath(slot.runtime);
		if (sessionPath) this.background.delete(sessionPath);
		slot.unsubscribe();
		slot.unsubscribePersistedEntries();
		slot.unsubscribeRunCompleted();
		try {
			await slot.runtime.dispose();
		} finally {
			this.hooks.runtimeDisposed?.(slot.runtime);
		}
	}

	private findLiveSlot(sessionPath: string): RuntimeSlot | undefined {
		if (this.getSessionPath(this.foreground.runtime) === sessionPath) return this.foreground;
		return this.background.get(sessionPath);
	}

	private isManagedSessionPath(sessionPath: string, sessionDir: string): boolean {
		if (extname(sessionPath) !== ".jsonl") return false;
		const pathRelativeToSessionDir = relative(sessionDir, sessionPath);
		return (
			pathRelativeToSessionDir.length > 0 &&
			pathRelativeToSessionDir !== ".." &&
			!pathRelativeToSessionDir.startsWith(`..${sep}`)
		);
	}

	private async openRuntime(
		sessionPath: string,
		previousSessionFile?: string,
		cwdOverride?: string,
		timings?: Record<string, number>,
	): Promise<AgentSessionRuntime> {
		const sessionManager = await SessionManager.openAsync(sessionPath, undefined, cwdOverride, {
			onTiming: (sessionTimings) => {
				if (!timings) return;
				timings.jsonlRead = sessionTimings.readMs;
				timings.jsonlParse = sessionTimings.parseMs;
				timings.sessionIndex = sessionTimings.indexMs;
			},
		});
		const runtimeStartedAt = performance.now();
		const runtime = await createAgentSessionRuntime(this.createRuntime, {
			cwd: sessionManager.getCwd(),
			agentDir: this.current.services.agentDir,
			sessionManager,
			sessionStartEvent: {
				type: "session_start",
				reason: "resume",
				previousSessionFile,
			},
		});
		if (timings) timings.runtimeCreate = performance.now() - runtimeStartedAt;
		return runtime;
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

	private enqueue<T>(operation: () => Promise<T>): Promise<T> {
		const next = this.transition.then(operation, operation);
		this.transition = next.then(
			() => {},
			() => {},
		);
		return next;
	}
}
