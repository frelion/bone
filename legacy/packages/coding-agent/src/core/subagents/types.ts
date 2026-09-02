import type {
	SubagentHandle,
	SubagentHandoff,
	SubagentRunStatus,
	SubagentRuntime,
	SubagentRuntimeEvent,
	SubagentScope,
	SubagentSessionStatus,
	SubagentYield,
} from "@frelion/bone-agent-core";

export interface SubagentOrigin {
	exchangeId: string;
	actionId: string;
	toolCallId: string;
}

export interface SubagentExecutionProjection {
	agentRef: string;
	label: string;
	scope: SubagentScope;
	status: SubagentSessionStatus;
	lastRunStatus?: SubagentRunStatus;
	origin: SubagentOrigin;
	handoff?: SubagentHandoff;
	yields: readonly SubagentYield[];
	unreadYieldCount: number;
	error?: string;
	createdAt: number;
	lastActivityAt: number;
}

export interface SubagentProjection {
	executions: readonly SubagentExecutionProjection[];
}

export type SubagentProjectionListener = (projection: SubagentProjection) => void;

export interface CodingSubagentManagerOptions {
	runtime: SubagentRuntime;
}

function executionLabel(handle: SubagentHandle): string {
	return handle.label?.trim() || handle.id;
}

export class CodingSubagentManager {
	readonly runtime: SubagentRuntime;
	private readonly executions = new Map<string, SubagentExecutionProjection>();
	private readonly pendingYields = new Map<string, SubagentYield[]>();
	private readonly readYieldSequences = new Map<string, number>();
	private readonly listeners = new Set<SubagentProjectionListener>();
	private readonly unsubscribeRuntime: () => void;
	private closingAll?: Promise<void>;

	constructor(options: CodingSubagentManagerOptions) {
		this.runtime = options.runtime;
		this.unsubscribeRuntime = this.runtime.subscribe((event) => this.handleRuntimeEvent(event));
	}

	get projection(): SubagentProjection {
		return { executions: [...this.executions.values()] };
	}

	register(handle: SubagentHandle, origin: SubagentOrigin): void {
		const pendingYields = this.pendingYields.get(handle.id) ?? [];
		const lastReadSequence = this.readYieldSequences.get(handle.id) ?? 0;
		this.executions.set(handle.id, {
			agentRef: handle.id,
			label: executionLabel(handle),
			scope: handle.scope,
			status: handle.status,
			...(handle.lastRunStatus ? { lastRunStatus: handle.lastRunStatus } : {}),
			origin,
			yields: pendingYields,
			unreadYieldCount: pendingYields.filter((yielded) => yielded.sequence > lastReadSequence).length,
			createdAt: handle.createdAt,
			lastActivityAt: handle.lastActivityAt,
		});
		this.pendingYields.delete(handle.id);
		this.emit();
	}

	recordHandoff(agentRef: string, handoff: SubagentHandoff): void {
		this.update(agentRef, (execution) => ({
			...execution,
			handoff,
			error: undefined,
		}));
	}

	recordYieldsRead(agentRef: string, yields: readonly SubagentYield[]): void {
		if (yields.length === 0) return;
		const lastSequence = Math.max(
			this.readYieldSequences.get(agentRef) ?? 0,
			...yields.map((yielded) => yielded.sequence),
		);
		this.readYieldSequences.set(agentRef, lastSequence);
		this.update(agentRef, (execution) => ({
			...execution,
			unreadYieldCount: execution.yields.filter((yielded) => yielded.sequence > lastSequence).length,
		}));
	}

	subscribe(listener: SubagentProjectionListener): () => void {
		this.listeners.add(listener);
		listener(this.projection);
		return () => this.listeners.delete(listener);
	}

	async closeExchangeScoped(exchangeId: string): Promise<void> {
		const refs = [...this.executions.values()]
			.filter((execution) => execution.scope === "exchange" && execution.origin.exchangeId === exchangeId)
			.map((execution) => execution.agentRef);
		await Promise.allSettled(refs.map(async (agentRef) => await this.runtime.close(agentRef)));
	}

	async closeAll(): Promise<void> {
		this.closingAll ??= (async () => {
			await Promise.allSettled(this.runtime.list().map(async (handle) => await this.runtime.close(handle.id)));
			this.unsubscribeRuntime();
		})();
		await this.closingAll;
	}

	private handleRuntimeEvent(event: SubagentRuntimeEvent): void {
		if (event.type === "session_created") return;
		if (event.type === "session_evicted") {
			this.executions.delete(event.handle.id);
			this.pendingYields.delete(event.handle.id);
			this.readYieldSequences.delete(event.handle.id);
			this.emit();
			return;
		}
		if (event.type === "yield_published") {
			const execution = this.executions.get(event.handle.id);
			if (!execution) {
				const pending = [...(this.pendingYields.get(event.handle.id) ?? []), event.yield].slice(-20);
				this.pendingYields.set(event.handle.id, pending);
				return;
			}
			this.update(event.handle.id, (current) => ({
				...current,
				status: event.handle.status,
				lastActivityAt: event.handle.lastActivityAt,
				yields: [...current.yields, event.yield].slice(-20),
				unreadYieldCount: [...current.yields, event.yield]
					.slice(-20)
					.filter((yielded) => yielded.sequence > (this.readYieldSequences.get(event.handle.id) ?? 0)).length,
			}));
			return;
		}
		if (event.type === "run_completed") {
			this.updateFromHandle(event.handle, { handoff: event.handoff, error: undefined });
			return;
		}
		if (event.type === "run_failed") {
			this.updateFromHandle(event.handle, { error: event.error });
			return;
		}
		if (event.type === "run_cancelled") {
			this.updateFromHandle(event.handle, { error: event.reason });
			return;
		}
		this.updateFromHandle(event.handle);
	}

	private updateFromHandle(
		handle: SubagentHandle,
		update: Partial<Pick<SubagentExecutionProjection, "handoff" | "error">> = {},
	): void {
		this.update(handle.id, (execution) => ({
			...execution,
			label: executionLabel(handle),
			status: handle.status,
			...(handle.lastRunStatus ? { lastRunStatus: handle.lastRunStatus } : {}),
			lastActivityAt: handle.lastActivityAt,
			...update,
		}));
	}

	private update(
		agentRef: string,
		apply: (execution: SubagentExecutionProjection) => SubagentExecutionProjection,
	): void {
		const execution = this.executions.get(agentRef);
		if (!execution) return;
		this.executions.set(agentRef, apply(execution));
		this.emit();
	}

	private emit(): void {
		const projection = this.projection;
		for (const listener of this.listeners) listener(projection);
	}
}
