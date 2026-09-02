export type ExchangeStatus = "running" | "completed" | "failed" | "interrupted";

export type ModelTurnStatus = "running" | "completed" | "failed" | "interrupted";

export type NarrativePhase = "commentary" | "final_answer";

export type NarrativeStatus = "streaming" | "completed" | "interrupted";

export type ActionStatus = "in_progress" | "completed" | "failed" | "cancelled";

export type ToolCallStatus = "in_progress" | "completed" | "failed" | "cancelled";

export type ExchangeInputDelivery = "prompt" | "follow_up" | "steer";

export type ExchangeStartDelivery = Exclude<ExchangeInputDelivery, "steer">;

export interface ExchangeInput {
	id: string;
	delivery: ExchangeInputDelivery;
	content: string;
	createdAt: number;
}

export interface ModelTurn {
	id: string;
	status: ModelTurnStatus;
	sequence: number;
	startedAt: number;
	completedAt?: number;
}

export interface NarrativeItem {
	type: "narrative";
	id: string;
	modelTurnId?: string;
	phase: NarrativePhase;
	status: NarrativeStatus;
	content: string;
	sequence: number;
	startedAt: number;
	completedAt?: number;
}

export interface ToolCallExecution {
	id: string;
	modelTurnId: string;
	toolName: string;
	status: ToolCallStatus;
	arguments: unknown;
	progress?: unknown;
	result?: unknown;
	error?: string;
	sequence: number;
	startedAt: number;
	completedAt?: number;
}

export interface ActionItem {
	type: "action";
	id: string;
	kind: string;
	label: string;
	status: ActionStatus;
	modelTurnIds: readonly string[];
	toolCalls: readonly ToolCallExecution[];
	outcome?: unknown;
	error?: string;
	sequence: number;
	startedAt: number;
	completedAt?: number;
}

export type ExchangeItem = NarrativeItem | ActionItem;

export interface Exchange {
	id: string;
	sessionId: string;
	status: ExchangeStatus;
	inputs: readonly ExchangeInput[];
	modelTurns: readonly ModelTurn[];
	items: readonly ExchangeItem[];
	startedAt: number;
	completedAt?: number;
	error?: string;
}

export interface ExchangeProjection {
	sessionId: string;
	exchanges: readonly Exchange[];
	activeExchangeId?: string;
}

interface ExchangeEventBase {
	exchangeId: string;
	at: number;
}

export type ExchangeProjectorEvent =
	| (ExchangeEventBase & {
			type: "exchange_started";
			input: Omit<ExchangeInput, "createdAt" | "delivery"> & { delivery: ExchangeStartDelivery };
	  })
	| (ExchangeEventBase & {
			type: "exchange_input_added";
			input: Omit<ExchangeInput, "createdAt"> & { delivery: "steer" | "follow_up" };
	  })
	| (ExchangeEventBase & {
			type: "model_turn_started";
			modelTurnId: string;
	  })
	| (ExchangeEventBase & {
			type: "model_turn_completed";
			modelTurnId: string;
			status?: Exclude<ModelTurnStatus, "running">;
	  })
	| (ExchangeEventBase & {
			type: "narrative_started";
			narrativeId: string;
			modelTurnId?: string;
			phase: NarrativePhase;
			content?: string;
	  })
	| (ExchangeEventBase & {
			type: "narrative_delta";
			narrativeId: string;
			delta: string;
	  })
	| (ExchangeEventBase & {
			type: "narrative_completed";
			narrativeId: string;
			status?: Exclude<NarrativeStatus, "streaming">;
	  })
	| (ExchangeEventBase & {
			type: "action_started";
			actionId: string;
			kind: string;
			label: string;
	  })
	| (ExchangeEventBase & {
			type: "action_tool_call_started";
			actionId: string;
			toolCallId: string;
			modelTurnId: string;
			toolName: string;
			arguments: unknown;
	  })
	| (ExchangeEventBase & {
			type: "action_tool_call_updated";
			actionId: string;
			toolCallId: string;
			progress: unknown;
	  })
	| (ExchangeEventBase & {
			type: "action_tool_call_completed";
			actionId: string;
			toolCallId: string;
			status?: Exclude<ToolCallStatus, "in_progress">;
			result?: unknown;
			error?: string;
	  })
	| (ExchangeEventBase & {
			type: "action_completed";
			actionId: string;
			status?: Exclude<ActionStatus, "in_progress">;
			outcome?: unknown;
			error?: string;
	  })
	| (ExchangeEventBase & {
			type: "exchange_completed";
			status?: "completed" | "failed" | "interrupted";
			error?: string;
	  });
