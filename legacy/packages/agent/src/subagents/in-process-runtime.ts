import type {
	SubagentHandle,
	SubagentHandoff,
	SubagentRunInput,
	SubagentRuntime,
	SubagentRuntimeEvent,
	SubagentRuntimeEventListener,
	SubagentScope,
	SubagentSession,
	SubagentSessionFactory,
	SubagentTask,
	SubagentYield,
	SubagentYieldInput,
} from "./types.ts";
import { SubagentRuntimeError } from "./types.ts";

interface SessionRecord {
	handle: SubagentHandle;
	task: SubagentTask;
	session?: SubagentSession;
	activePromise?: Promise<SubagentHandoff>;
	activeRunId?: string;
	runAbortController?: AbortController;
	cancelPromise?: Promise<void>;
	closePromise?: Promise<void>;
	cancelRequested: boolean;
	cancelReason?: string;
	lastHandoff?: SubagentHandoff;
	lastError?: Error;
	runSequence: number;
	yieldSequence: number;
	yieldMailbox: SubagentYield[];
}

export interface InProcessSubagentRuntimeOptions {
	createSession: SubagentSessionFactory;
	createId?: () => string;
	now?: () => number;
	/** Maximum child runs active at once. Default: 4. */
	maxConcurrentChildren?: number;
	/** Maximum non-closed child sessions retained at once. Default: 16. */
	maxRetainedSessions?: number;
	/** Maximum closed child records retained for history. Default: 32. */
	maxClosedSessions?: number;
}

function toError(error: unknown): Error {
	return error instanceof Error ? error : new Error(String(error));
}

function cancellationError(reason: unknown): SubagentRuntimeError {
	return new SubagentRuntimeError(
		"cancelled",
		typeof reason === "string" && reason ? reason : "Subagent operation was cancelled",
	);
}

function raceWithSignal<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
	if (signal.aborted) return Promise.reject(cancellationError(signal.reason));
	return new Promise<T>((resolve, reject) => {
		const onAbort = () => reject(cancellationError(signal.reason));
		signal.addEventListener("abort", onAbort, { once: true });
		promise.then(resolve, reject).finally(() => signal.removeEventListener("abort", onAbort));
	});
}

function boundText(value: string, maxLength: number): string {
	const text = value.trim();
	return text.length <= maxLength ? text : `${text.slice(0, maxLength - 1)}…`;
}

function boundList(values: readonly string[] | undefined, maxItems: number): readonly string[] | undefined {
	if (!values) return undefined;
	return values.slice(0, maxItems).map((value) => boundText(value, 500));
}

function boundHandoff(handoff: SubagentHandoff): SubagentHandoff {
	// Keep the complete structured payload near 24 KB in the worst case so a
	// child cannot erase the parent-context savings with an oversized handoff.
	return {
		status: handoff.status,
		summary: boundText(handoff.summary, 4_000),
		...(handoff.decisions ? { decisions: boundList(handoff.decisions, 5) } : {}),
		...(handoff.changedFiles ? { changedFiles: boundList(handoff.changedFiles, 10) } : {}),
		...(handoff.validations ? { validations: boundList(handoff.validations, 5) } : {}),
		...(handoff.risks ? { risks: boundList(handoff.risks, 5) } : {}),
		...(handoff.artifactRefs ? { artifactRefs: boundList(handoff.artifactRefs, 10) } : {}),
	};
}

const MAX_YIELD_MAILBOX_ITEMS = 20;
const MAX_YIELD_MESSAGE_LENGTH = 1_000;
const MAX_YIELD_ARTIFACT_REFS = 5;
const MAX_YIELD_ARTIFACT_REF_LENGTH = 250;

function boundYieldInput(input: SubagentYieldInput): Pick<SubagentYield, "kind" | "message" | "artifactRefs"> {
	const message = boundText(input.message, MAX_YIELD_MESSAGE_LENGTH);
	return {
		kind: input.kind,
		message: message || "Subagent update",
		...(input.artifactRefs
			? {
					artifactRefs: input.artifactRefs
						.slice(0, MAX_YIELD_ARTIFACT_REFS)
						.map((ref) => boundText(ref, MAX_YIELD_ARTIFACT_REF_LENGTH))
						.filter(Boolean),
				}
			: {}),
	};
}

/**
 * Hosts persistent child-agent sessions in the current process.
 *
 * Delegation starts asynchronously and returns a handle immediately. Child
 * events stay on this runtime's event stream; only explicit handoffs returned
 * by wait/ask need to enter the parent model context.
 */
export class InProcessSubagentRuntime implements SubagentRuntime {
	private readonly records = new Map<string, SessionRecord>();
	private readonly listeners = new Set<SubagentRuntimeEventListener>();
	private readonly createSession: SubagentSessionFactory;
	private readonly createId: () => string;
	private readonly now: () => number;
	private readonly maxConcurrentChildren: number;
	private readonly maxRetainedSessions: number;
	private readonly maxClosedSessions: number;

	constructor(options: InProcessSubagentRuntimeOptions) {
		if (
			options.maxConcurrentChildren !== undefined &&
			(!Number.isInteger(options.maxConcurrentChildren) || options.maxConcurrentChildren < 1)
		) {
			throw new SubagentRuntimeError("invalid_argument", "maxConcurrentChildren must be a positive integer");
		}
		if (
			options.maxRetainedSessions !== undefined &&
			(!Number.isInteger(options.maxRetainedSessions) || options.maxRetainedSessions < 1)
		) {
			throw new SubagentRuntimeError("invalid_argument", "maxRetainedSessions must be a positive integer");
		}
		if (
			options.maxClosedSessions !== undefined &&
			(!Number.isInteger(options.maxClosedSessions) || options.maxClosedSessions < 0)
		) {
			throw new SubagentRuntimeError("invalid_argument", "maxClosedSessions must be a non-negative integer");
		}
		this.createSession = options.createSession;
		this.createId = options.createId ?? (() => globalThis.crypto.randomUUID());
		this.now = options.now ?? Date.now;
		this.maxConcurrentChildren = options.maxConcurrentChildren ?? 4;
		this.maxRetainedSessions = options.maxRetainedSessions ?? 16;
		this.maxClosedSessions = options.maxClosedSessions ?? 32;
	}

	async delegate(task: SubagentTask, signal?: AbortSignal): Promise<SubagentHandle> {
		if (signal?.aborted) throw cancellationError(signal.reason);
		const objective = task.objective.trim();
		if (!objective) {
			throw new SubagentRuntimeError("invalid_argument", "Subagent objective must not be empty");
		}
		const records = [...this.records.values()];
		this.assertConcurrencyAvailable(records);
		if (records.filter((record) => record.handle.status !== "closed").length >= this.maxRetainedSessions) {
			throw new SubagentRuntimeError(
				"busy",
				`Subagent retained-session limit reached (${this.maxRetainedSessions}); close an existing agent first`,
			);
		}
		const id = this.createId();
		if (!id || this.records.has(id)) {
			throw new SubagentRuntimeError("invalid_argument", `Subagent ID must be unique and non-empty: ${id}`);
		}
		const at = this.now();
		const normalizedTask: SubagentTask = { ...task, objective };
		const record: SessionRecord = {
			handle: {
				id,
				...(task.label ? { label: task.label } : {}),
				scope: task.scope ?? "exchange",
				status: "starting",
				createdAt: at,
				lastActivityAt: at,
			},
			task: normalizedTask,
			cancelRequested: false,
			runSequence: 0,
			yieldSequence: 0,
			yieldMailbox: [],
		};
		this.records.set(id, record);
		this.emit({ type: "session_created", handle: this.snapshot(record), task: normalizedTask });
		this.startRun(record, { kind: "delegation", text: objective });
		if (signal?.aborted) {
			void this.cancel(id, typeof signal.reason === "string" ? signal.reason : undefined);
			throw cancellationError(signal.reason);
		}
		return this.snapshot(record);
	}

	async ask(agentRef: string, question: string, signal?: AbortSignal): Promise<SubagentHandoff> {
		const record = this.requireRecord(agentRef);
		const text = question.trim();
		if (!text) throw new SubagentRuntimeError("invalid_argument", "Subagent question must not be empty");
		if (record.handle.status === "closing" || record.handle.status === "closed") {
			throw new SubagentRuntimeError("closed", `Subagent session is closed: ${agentRef}`);
		}
		if (record.handle.status === "cancelling") {
			throw new SubagentRuntimeError("busy", `Subagent session is being cancelled: ${agentRef}`);
		}
		if (record.activePromise) {
			throw new SubagentRuntimeError("busy", `Subagent session is busy: ${agentRef}`);
		}
		if (record.handle.status === "failed") {
			throw new SubagentRuntimeError("failed", `Subagent session failed: ${agentRef}`, {
				cause: record.lastError,
			});
		}
		this.assertConcurrencyAvailable([...this.records.values()]);
		const promise = this.startRun(record, { kind: "question", text });
		return await this.awaitRun(record, promise, signal);
	}

	async wait(agentRef: string, signal?: AbortSignal): Promise<SubagentHandoff> {
		const record = this.requireRecord(agentRef);
		if (record.activePromise) return await this.awaitRun(record, record.activePromise, signal);
		if (record.lastHandoff) return record.lastHandoff;
		if (record.handle.lastRunStatus === "cancelled") {
			throw new SubagentRuntimeError("cancelled", `Subagent run was cancelled: ${agentRef}`);
		}
		if (record.lastError) {
			throw new SubagentRuntimeError("failed", `Subagent run failed: ${agentRef}`, { cause: record.lastError });
		}
		if (record.handle.status === "closed") {
			throw new SubagentRuntimeError("closed", `Subagent session is closed: ${agentRef}`);
		}
		throw new SubagentRuntimeError("failed", `Subagent has no result: ${agentRef}`);
	}

	async cancel(agentRef: string, reason = "Cancelled by parent agent"): Promise<SubagentHandle> {
		const record = this.requireRecord(agentRef);
		if (record.handle.status === "closed") return this.snapshot(record);
		if (record.cancelPromise) {
			await record.cancelPromise;
			return this.snapshot(record);
		}
		if (!record.activePromise || !record.activeRunId) return this.snapshot(record);

		record.cancelRequested = true;
		record.cancelReason = reason;
		record.runAbortController?.abort(reason);
		const activePromise = record.activePromise;
		if (record.handle.status !== "closing") this.updateHandle(record, { status: "cancelling" });
		const cancelPromise = Promise.resolve().then(async () => {
			let abortError: Error | undefined;
			if (record.session) {
				try {
					await record.session.abort(reason);
				} catch (error) {
					abortError = toError(error);
				}
			}
			try {
				await activePromise;
			} catch {
				// Cancellation is reported through run state and the event stream.
			}
			if (abortError) {
				throw new SubagentRuntimeError("failed", `Failed to abort subagent: ${agentRef}`, { cause: abortError });
			}
			if (record.handle.status === "cancelling") this.updateHandle(record, { status: "idle" });
		});
		record.cancelPromise = cancelPromise;
		try {
			await cancelPromise;
		} finally {
			if (record.cancelPromise === cancelPromise) record.cancelPromise = undefined;
		}
		return this.snapshot(record);
	}

	async close(agentRef: string): Promise<void> {
		const record = this.requireRecord(agentRef);
		if (record.handle.status === "closed") return;
		if (record.closePromise) return await record.closePromise;
		this.updateHandle(record, { status: "closing" });
		const closePromise = Promise.resolve().then(async () => {
			try {
				await this.cancel(agentRef, "Subagent session closed");
				await record.session?.close?.();
				// Retain only bounded projection data after close, not the
				// child harness and its private transcript.
				record.session = undefined;
				this.updateHandle(record, { status: "closed" });
				this.emit({ type: "session_closed", handle: this.snapshot(record) });
				this.evictClosedSessions();
			} catch (error) {
				this.updateHandle(record, { status: "failed" });
				throw new SubagentRuntimeError("failed", `Failed to close subagent: ${agentRef}`, {
					cause: toError(error),
				});
			}
		});
		record.closePromise = closePromise;
		try {
			await closePromise;
		} finally {
			if (record.closePromise === closePromise) record.closePromise = undefined;
		}
	}

	async closeAll(scope?: SubagentScope): Promise<void> {
		const records = [...this.records.values()].filter(
			(record) => record.handle.status !== "closed" && (!scope || record.handle.scope === scope),
		);
		await Promise.all(records.map((record) => this.close(record.handle.id)));
	}

	drainYields(agentRef: string): SubagentYield[] {
		const record = this.requireRecord(agentRef);
		return record.yieldMailbox.splice(0);
	}

	get(agentRef: string): SubagentHandle | undefined {
		const record = this.records.get(agentRef);
		return record ? this.snapshot(record) : undefined;
	}

	list(): SubagentHandle[] {
		return [...this.records.values()].map((record) => this.snapshot(record));
	}

	subscribe(listener: SubagentRuntimeEventListener): () => void {
		this.listeners.add(listener);
		return () => this.listeners.delete(listener);
	}

	private startRun(record: SessionRecord, input: SubagentRunInput): Promise<SubagentHandoff> {
		if (record.activePromise) {
			throw new SubagentRuntimeError("busy", `Subagent session is busy: ${record.handle.id}`);
		}
		record.cancelRequested = false;
		record.cancelReason = undefined;
		record.lastError = undefined;
		const runId = `${record.handle.id}:${++record.runSequence}`;
		record.activeRunId = runId;
		const controller = new AbortController();
		record.runAbortController = controller;
		let resolveRun!: (handoff: SubagentHandoff) => void;
		let rejectRun!: (error: unknown) => void;
		const promise = new Promise<SubagentHandoff>((resolve, reject) => {
			resolveRun = resolve;
			rejectRun = reject;
		});
		record.activePromise = promise;
		// Install the busy marker before factories, sessions, or synchronous
		// event listeners can re-enter the runtime.
		void this.executeRun(record, runId, input, controller.signal).then(resolveRun, rejectRun);
		void promise.catch(() => {});
		return promise;
	}

	private async executeRun(
		record: SessionRecord,
		runId: string,
		input: SubagentRunInput,
		signal: AbortSignal,
	): Promise<SubagentHandoff> {
		try {
			if (!record.session) {
				const creation = Promise.resolve(
					this.createSession({
						handle: this.snapshot(record),
						task: record.task,
						signal,
						publishYield: (input) => this.publishYield(record, input),
					}),
				);
				let session: SubagentSession;
				try {
					session = await raceWithSignal(creation, signal);
				} catch (error) {
					// A factory may ignore cancellation and finish later. Do not
					// attach its stale session, but dispose it when it arrives.
					void creation.then((lateSession) => lateSession.close?.()).catch(() => {});
					throw error;
				}
				if (signal.aborted) {
					void session.close?.();
					throw cancellationError(signal.reason);
				}
				record.session = session;
			}
			if (record.cancelRequested || signal.aborted) {
				throw cancellationError(record.cancelReason ?? signal.reason);
			}

			this.updateHandle(record, { status: "running" });
			this.emit({ type: "run_started", handle: this.snapshot(record), runId, input });
			const handoff = boundHandoff(await record.session.run(input, signal));
			if (record.cancelRequested || signal.aborted) {
				throw cancellationError(record.cancelReason ?? signal.reason);
			}

			record.lastHandoff = handoff;
			if (handoff.status === "failed") {
				record.lastError = new Error(handoff.summary || "Subagent returned a failed handoff");
				this.updateHandle(record, { status: "failed", lastRunStatus: "failed" });
				this.emit({
					type: "run_failed",
					handle: this.snapshot(record),
					runId,
					error: record.lastError.message,
				});
				return handoff;
			}
			this.updateHandle(record, {
				status: record.handle.status === "closing" ? "closing" : "idle",
				lastRunStatus: "completed",
			});
			this.emit({ type: "run_completed", handle: this.snapshot(record), runId, handoff });
			return handoff;
		} catch (error) {
			const cause = toError(error);
			if (record.cancelRequested || (cause instanceof SubagentRuntimeError && cause.code === "cancelled")) {
				const reason = record.cancelReason ?? cause.message;
				record.lastHandoff = undefined;
				this.updateHandle(record, {
					status:
						record.handle.status === "closing"
							? "closing"
							: record.handle.status === "cancelling"
								? "cancelling"
								: "idle",
					lastRunStatus: "cancelled",
				});
				this.emit({ type: "run_cancelled", handle: this.snapshot(record), runId, reason });
				throw new SubagentRuntimeError("cancelled", reason, { cause });
			}

			record.lastHandoff = undefined;
			record.lastError = cause;
			this.updateHandle(record, { status: "failed", lastRunStatus: "failed" });
			this.emit({
				type: "run_failed",
				handle: this.snapshot(record),
				runId,
				error: cause.message,
			});
			throw new SubagentRuntimeError("failed", `Subagent run failed: ${record.handle.id}`, { cause });
		} finally {
			if (record.activeRunId === runId) {
				record.activePromise = undefined;
				record.activeRunId = undefined;
				record.runAbortController = undefined;
				record.cancelRequested = false;
				record.cancelReason = undefined;
			}
		}
	}

	private async awaitRun(
		record: SessionRecord,
		promise: Promise<SubagentHandoff>,
		signal?: AbortSignal,
	): Promise<SubagentHandoff> {
		if (!signal) return await promise;
		if (signal.aborted) {
			void this.cancel(record.handle.id, typeof signal.reason === "string" ? signal.reason : undefined);
			throw cancellationError(signal.reason);
		}
		const onAbort = () => {
			void this.cancel(record.handle.id, typeof signal.reason === "string" ? signal.reason : undefined);
		};
		signal.addEventListener("abort", onAbort, { once: true });
		try {
			return await raceWithSignal(promise, signal);
		} finally {
			signal.removeEventListener("abort", onAbort);
		}
	}

	private requireRecord(agentRef: string): SessionRecord {
		const record = this.records.get(agentRef);
		if (!record) throw new SubagentRuntimeError("unknown_agent", `Unknown subagent: ${agentRef}`);
		return record;
	}

	private publishYield(record: SessionRecord, input: SubagentYieldInput): void {
		if (
			!record.activeRunId ||
			record.cancelRequested ||
			record.handle.status === "cancelling" ||
			record.handle.status === "closing" ||
			record.handle.status === "closed"
		) {
			return;
		}
		const bounded = boundYieldInput(input);
		const createdAt = this.now();
		const yielded: SubagentYield = {
			agentRef: record.handle.id,
			sequence: ++record.yieldSequence,
			kind: bounded.kind,
			message: bounded.message,
			...(bounded.artifactRefs ? { artifactRefs: bounded.artifactRefs } : {}),
			createdAt,
		};
		record.handle = { ...record.handle, lastActivityAt: createdAt };
		record.yieldMailbox.push(yielded);
		if (record.yieldMailbox.length > MAX_YIELD_MAILBOX_ITEMS) {
			record.yieldMailbox.splice(0, record.yieldMailbox.length - MAX_YIELD_MAILBOX_ITEMS);
		}
		this.emit({
			type: "yield_published",
			handle: this.snapshot(record),
			yield: {
				...yielded,
				...(yielded.artifactRefs ? { artifactRefs: [...yielded.artifactRefs] } : {}),
			},
		});
	}

	private evictClosedSessions(): void {
		const closed = [...this.records.values()]
			.filter((record) => record.handle.status === "closed")
			.sort(
				(left, right) =>
					left.handle.createdAt - right.handle.createdAt || left.handle.id.localeCompare(right.handle.id),
			);
		const excess = closed.length - this.maxClosedSessions;
		if (excess <= 0) return;
		for (const record of closed.slice(0, excess)) {
			if (!this.records.delete(record.handle.id)) continue;
			this.emit({ type: "session_evicted", handle: this.snapshot(record) });
		}
	}

	private assertConcurrencyAvailable(records: readonly SessionRecord[]): void {
		if (records.filter((record) => record.activePromise).length < this.maxConcurrentChildren) return;
		throw new SubagentRuntimeError("busy", `Subagent concurrency limit reached (${this.maxConcurrentChildren})`);
	}

	private updateHandle(
		record: SessionRecord,
		update: Pick<SubagentHandle, "status"> & Partial<Pick<SubagentHandle, "lastRunStatus">>,
	): void {
		record.handle = {
			...record.handle,
			...update,
			lastActivityAt: this.now(),
		};
	}

	private snapshot(record: SessionRecord): SubagentHandle {
		return { ...record.handle };
	}

	private emit(event: SubagentRuntimeEvent): void {
		for (const listener of this.listeners) {
			try {
				listener(event);
			} catch {
				// Observers must not change child-agent lifecycle behavior.
			}
		}
	}
}
