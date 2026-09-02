export type SubagentScope = "exchange" | "conversation";

export type SubagentSessionStatus = "starting" | "running" | "idle" | "failed" | "cancelling" | "closing" | "closed";

export type SubagentRunStatus = "completed" | "failed" | "cancelled";

export type SubagentHandoffStatus = "completed" | "partial" | "failed";

export type SubagentYieldKind = "progress" | "finding" | "risk" | "proposal";

export interface SubagentYieldInput {
	kind: SubagentYieldKind;
	message: string;
	artifactRefs?: readonly string[];
}

export interface SubagentYield extends SubagentYieldInput {
	agentRef: string;
	sequence: number;
	createdAt: number;
}

export interface SubagentTask {
	objective: string;
	label?: string;
	scope?: SubagentScope;
	contextRefs?: readonly string[];
	expectedOutput?: string;
	metadata?: Readonly<Record<string, unknown>>;
}

export interface SubagentHandoff {
	status: SubagentHandoffStatus;
	summary: string;
	decisions?: readonly string[];
	changedFiles?: readonly string[];
	validations?: readonly string[];
	risks?: readonly string[];
	artifactRefs?: readonly string[];
}

export interface SubagentHandle {
	id: string;
	label?: string;
	scope: SubagentScope;
	status: SubagentSessionStatus;
	lastRunStatus?: SubagentRunStatus;
	createdAt: number;
	lastActivityAt: number;
}

export interface SubagentRunInput {
	kind: "delegation" | "question";
	text: string;
}

/**
 * One persistent child-agent context. Each call to run starts a new Exchange
 * while retaining the session's prior context.
 */
export interface SubagentSession {
	run(input: SubagentRunInput, signal?: AbortSignal): Promise<SubagentHandoff>;
	abort(reason: string): Promise<void>;
	close?(): Promise<void>;
}

export interface SubagentSessionFactoryContext {
	handle: SubagentHandle;
	task: SubagentTask;
	signal: AbortSignal;
	/** Publish a bounded parent-visible update without blocking child execution. */
	publishYield(input: SubagentYieldInput): void;
}

export type SubagentSessionFactory = (
	context: SubagentSessionFactoryContext,
) => Promise<SubagentSession> | SubagentSession;

export type SubagentRuntimeEvent =
	| { type: "session_created"; handle: SubagentHandle; task: SubagentTask }
	| { type: "run_started"; handle: SubagentHandle; runId: string; input: SubagentRunInput }
	| { type: "run_completed"; handle: SubagentHandle; runId: string; handoff: SubagentHandoff }
	| { type: "run_failed"; handle: SubagentHandle; runId: string; error: string }
	| { type: "run_cancelled"; handle: SubagentHandle; runId: string; reason: string }
	| { type: "yield_published"; handle: SubagentHandle; yield: SubagentYield }
	| { type: "session_closed"; handle: SubagentHandle }
	| { type: "session_evicted"; handle: SubagentHandle };

export type SubagentRuntimeEventListener = (event: SubagentRuntimeEvent) => void;

export interface SubagentRuntime {
	delegate(task: SubagentTask, signal?: AbortSignal): Promise<SubagentHandle>;
	ask(agentRef: string, question: string, signal?: AbortSignal): Promise<SubagentHandoff>;
	wait(agentRef: string, signal?: AbortSignal): Promise<SubagentHandoff>;
	cancel(agentRef: string, reason?: string): Promise<SubagentHandle>;
	close(agentRef: string): Promise<void>;
	closeAll(scope?: SubagentScope): Promise<void>;
	/** Atomically consume all currently unread bounded child updates. */
	drainYields(agentRef: string): SubagentYield[];
	get(agentRef: string): SubagentHandle | undefined;
	list(): SubagentHandle[];
	subscribe(listener: SubagentRuntimeEventListener): () => void;
}

export type SubagentRuntimeErrorCode =
	| "unknown_agent"
	| "invalid_argument"
	| "busy"
	| "failed"
	| "cancelled"
	| "closed";

export class SubagentRuntimeError extends Error {
	readonly code: SubagentRuntimeErrorCode;

	constructor(code: SubagentRuntimeErrorCode, message: string, options?: ErrorOptions) {
		super(message, options);
		this.name = "SubagentRuntimeError";
		this.code = code;
	}
}
