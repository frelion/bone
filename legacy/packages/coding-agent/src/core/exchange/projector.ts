import type {
	ActionItem,
	Exchange,
	ExchangeItem,
	ExchangeProjection,
	ExchangeProjectorEvent,
	ModelTurn,
	NarrativeItem,
	ToolCallExecution,
} from "./types.ts";

export class ExchangeProjectionError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "ExchangeProjectionError";
	}
}

export function createExchangeProjection(sessionId: string): ExchangeProjection {
	if (!sessionId) throw new ExchangeProjectionError("Session ID must not be empty");
	return { sessionId, exchanges: [] };
}

export function shouldShowWorking(exchange: Exchange): boolean {
	if (exchange.status !== "running") return false;
	if (exchange.items.some((item) => isAction(item) && item.status === "in_progress")) return false;
	return !exchange.items.some((item) => isNarrative(item) && item.phase === "final_answer");
}

export function getActiveActions(exchange: Exchange): readonly ActionItem[] {
	return exchange.items.filter((item): item is ActionItem => item.type === "action" && item.status === "in_progress");
}

export function projectExchangeEvent(
	projection: ExchangeProjection,
	event: ExchangeProjectorEvent,
): ExchangeProjection {
	if (event.type === "exchange_started") {
		if (projection.exchanges.some((exchange) => exchange.id === event.exchangeId)) {
			throw new ExchangeProjectionError(`Exchange already exists: ${event.exchangeId}`);
		}
		const activeExchange = getActiveExchange(projection);
		if (activeExchange) {
			throw new ExchangeProjectionError(
				`Cannot start exchange ${event.exchangeId} while ${activeExchange.id} is running`,
			);
		}
		const exchange: Exchange = {
			id: event.exchangeId,
			sessionId: projection.sessionId,
			status: "running",
			inputs: [{ ...event.input, createdAt: event.at }],
			modelTurns: [],
			items: [],
			startedAt: event.at,
		};
		return {
			sessionId: projection.sessionId,
			exchanges: [...projection.exchanges, exchange],
			activeExchangeId: exchange.id,
		};
	}

	const exchangeIndex = projection.exchanges.findIndex((exchange) => exchange.id === event.exchangeId);
	if (exchangeIndex === -1) {
		throw new ExchangeProjectionError(`Unknown exchange: ${event.exchangeId}`);
	}
	const exchange = projection.exchanges[exchangeIndex]!;
	if (exchange.status !== "running") {
		throw new ExchangeProjectionError(`Exchange is not running: ${event.exchangeId}`);
	}

	let nextExchange: Exchange;
	switch (event.type) {
		case "exchange_input_added":
			assertBeforeFinalAnswer(exchange, "add input");
			if (exchange.inputs.some((input) => input.id === event.input.id)) {
				throw new ExchangeProjectionError(`Exchange input already exists: ${event.input.id}`);
			}
			nextExchange = {
				...exchange,
				inputs: [...exchange.inputs, { ...event.input, createdAt: event.at }],
			};
			break;
		case "model_turn_started": {
			assertBeforeFinalAnswer(exchange, "start model turn");
			if (exchange.modelTurns.some((turn) => turn.id === event.modelTurnId)) {
				throw new ExchangeProjectionError(`Model turn already exists: ${event.modelTurnId}`);
			}
			if (exchange.modelTurns.some((turn) => turn.status === "running")) {
				throw new ExchangeProjectionError(`Exchange ${exchange.id} already has a running model turn`);
			}
			const turn: ModelTurn = {
				id: event.modelTurnId,
				status: "running",
				sequence: exchange.modelTurns.length,
				startedAt: event.at,
			};
			nextExchange = { ...exchange, modelTurns: [...exchange.modelTurns, turn] };
			break;
		}
		case "model_turn_completed":
			nextExchange = updateModelTurn(exchange, event.modelTurnId, (turn) => {
				if (turn.status !== "running") {
					throw new ExchangeProjectionError(`Model turn is not running: ${turn.id}`);
				}
				const activeToolCall = findActiveToolCallForTurn(exchange, turn.id);
				if (activeToolCall) {
					throw new ExchangeProjectionError(
						`Cannot complete model turn ${turn.id} with active tool call: ${activeToolCall.id}`,
					);
				}
				return { ...turn, status: event.status ?? "completed", completedAt: event.at };
			});
			break;
		case "narrative_started": {
			assertUniqueItem(exchange, event.narrativeId);
			if (event.phase === "commentary") assertBeforeFinalAnswer(exchange, "start commentary");
			if (event.modelTurnId) assertModelTurnRunning(exchange, event.modelTurnId);
			if (event.phase === "final_answer") {
				const activeAction = getActiveActions(exchange)[0];
				if (activeAction) {
					throw new ExchangeProjectionError(`Cannot start final answer with active action: ${activeAction.id}`);
				}
				if (exchange.items.some((item) => isNarrative(item) && item.phase === "final_answer")) {
					throw new ExchangeProjectionError(`Exchange ${exchange.id} already has a final answer`);
				}
			}
			const narrative: NarrativeItem = {
				type: "narrative",
				id: event.narrativeId,
				...(event.modelTurnId ? { modelTurnId: event.modelTurnId } : {}),
				phase: event.phase,
				status: "streaming",
				content: event.content ?? "",
				sequence: exchange.items.length,
				startedAt: event.at,
			};
			nextExchange = { ...exchange, items: [...exchange.items, narrative] };
			break;
		}
		case "narrative_delta":
			nextExchange = updateNarrative(exchange, event.narrativeId, (narrative) => {
				if (narrative.status !== "streaming") {
					throw new ExchangeProjectionError(`Narrative is not streaming: ${narrative.id}`);
				}
				return { ...narrative, content: narrative.content + event.delta };
			});
			break;
		case "narrative_completed":
			nextExchange = updateNarrative(exchange, event.narrativeId, (narrative) => {
				if (narrative.status !== "streaming") {
					throw new ExchangeProjectionError(`Narrative is not streaming: ${narrative.id}`);
				}
				return { ...narrative, status: event.status ?? "completed", completedAt: event.at };
			});
			break;
		case "action_started": {
			assertBeforeFinalAnswer(exchange, "start action");
			assertUniqueItem(exchange, event.actionId);
			const action: ActionItem = {
				type: "action",
				id: event.actionId,
				kind: event.kind,
				label: event.label,
				status: "in_progress",
				modelTurnIds: [],
				toolCalls: [],
				sequence: exchange.items.length,
				startedAt: event.at,
			};
			nextExchange = { ...exchange, items: [...exchange.items, action] };
			break;
		}
		case "action_tool_call_started":
			nextExchange = updateAction(exchange, event.actionId, (action) => {
				if (action.status !== "in_progress") {
					throw new ExchangeProjectionError(`Action is not in progress: ${action.id}`);
				}
				assertBeforeFinalAnswer(exchange, "start tool call");
				assertModelTurnRunning(exchange, event.modelTurnId);
				assertUniqueToolCall(exchange, event.toolCallId);
				const toolCall: ToolCallExecution = {
					id: event.toolCallId,
					modelTurnId: event.modelTurnId,
					toolName: event.toolName,
					status: "in_progress",
					arguments: event.arguments,
					sequence: action.toolCalls.length,
					startedAt: event.at,
				};
				return {
					...action,
					modelTurnIds: action.modelTurnIds.includes(event.modelTurnId)
						? action.modelTurnIds
						: [...action.modelTurnIds, event.modelTurnId],
					toolCalls: [...action.toolCalls, toolCall],
				};
			});
			break;
		case "action_tool_call_updated":
			nextExchange = updateToolCall(exchange, event.actionId, event.toolCallId, (action, toolCall) => {
				assertActionAndToolCallActive(action, toolCall);
				return { ...toolCall, progress: event.progress };
			});
			break;
		case "action_tool_call_completed":
			nextExchange = updateToolCall(exchange, event.actionId, event.toolCallId, (action, toolCall) => {
				assertActionAndToolCallActive(action, toolCall);
				return {
					...toolCall,
					status: event.status ?? "completed",
					...(event.result !== undefined ? { result: event.result } : {}),
					...(event.error !== undefined ? { error: event.error } : {}),
					completedAt: event.at,
				};
			});
			break;
		case "action_completed":
			nextExchange = updateAction(exchange, event.actionId, (action) => {
				if (action.status !== "in_progress") {
					throw new ExchangeProjectionError(`Action is not in progress: ${action.id}`);
				}
				const activeToolCall = action.toolCalls.find((toolCall) => toolCall.status === "in_progress");
				if (activeToolCall) {
					throw new ExchangeProjectionError(
						`Cannot complete action ${action.id} with active tool call: ${activeToolCall.id}`,
					);
				}
				return {
					...action,
					status: event.status ?? "completed",
					...(event.outcome !== undefined ? { outcome: event.outcome } : {}),
					...(event.error !== undefined ? { error: event.error } : {}),
					completedAt: event.at,
				};
			});
			break;
		case "exchange_completed": {
			const runningTurn = exchange.modelTurns.find((turn) => turn.status === "running");
			if (runningTurn) {
				throw new ExchangeProjectionError(`Cannot complete exchange with running model turn: ${runningTurn.id}`);
			}
			const activeAction = exchange.items.find((item) => isAction(item) && item.status === "in_progress");
			if (activeAction) {
				throw new ExchangeProjectionError(`Cannot complete exchange with active action: ${activeAction.id}`);
			}
			const streamingNarrative = exchange.items.find((item) => isNarrative(item) && item.status === "streaming");
			if (streamingNarrative) {
				throw new ExchangeProjectionError(
					`Cannot complete exchange with streaming narrative: ${streamingNarrative.id}`,
				);
			}
			nextExchange = {
				...exchange,
				status: event.status ?? "completed",
				...(event.error !== undefined ? { error: event.error } : {}),
				completedAt: event.at,
			};
			break;
		}
	}

	const exchanges = [...projection.exchanges];
	exchanges[exchangeIndex] = nextExchange;
	return {
		sessionId: projection.sessionId,
		exchanges,
		...(nextExchange.status === "running" ? { activeExchangeId: nextExchange.id } : {}),
	};
}

function getActiveExchange(projection: ExchangeProjection): Exchange | undefined {
	if (!projection.activeExchangeId) return undefined;
	return projection.exchanges.find((exchange) => exchange.id === projection.activeExchangeId);
}

function isNarrative(item: ExchangeItem): item is NarrativeItem {
	return item.type === "narrative";
}

function isAction(item: ExchangeItem): item is ActionItem {
	return item.type === "action";
}

function assertUniqueItem(exchange: Exchange, itemId: string): void {
	if (exchange.items.some((item) => item.id === itemId)) {
		throw new ExchangeProjectionError(`Exchange item already exists: ${itemId}`);
	}
}

function assertModelTurnRunning(exchange: Exchange, modelTurnId: string): void {
	const turn = exchange.modelTurns.find((candidate) => candidate.id === modelTurnId);
	if (!turn) {
		throw new ExchangeProjectionError(`Unknown model turn: ${modelTurnId}`);
	}
	if (turn.status !== "running") {
		throw new ExchangeProjectionError(`Model turn is not running: ${modelTurnId}`);
	}
}

function assertBeforeFinalAnswer(exchange: Exchange, operation: string): void {
	if (exchange.items.some((item) => isNarrative(item) && item.phase === "final_answer")) {
		throw new ExchangeProjectionError(`Cannot ${operation} after final answer started`);
	}
}

function updateModelTurn(exchange: Exchange, id: string, update: (turn: ModelTurn) => ModelTurn): Exchange {
	const index = exchange.modelTurns.findIndex((turn) => turn.id === id);
	if (index === -1) throw new ExchangeProjectionError(`Unknown model turn: ${id}`);
	const modelTurns = [...exchange.modelTurns];
	modelTurns[index] = update(modelTurns[index]!);
	return { ...exchange, modelTurns };
}

function updateNarrative(
	exchange: Exchange,
	id: string,
	update: (narrative: NarrativeItem) => NarrativeItem,
): Exchange {
	const index = exchange.items.findIndex((item) => item.id === id);
	const item = exchange.items[index];
	if (!item || !isNarrative(item)) throw new ExchangeProjectionError(`Unknown narrative: ${id}`);
	const items = [...exchange.items];
	items[index] = update(item);
	return { ...exchange, items };
}

function updateAction(exchange: Exchange, id: string, update: (action: ActionItem) => ActionItem): Exchange {
	const index = exchange.items.findIndex((item) => item.id === id);
	const item = exchange.items[index];
	if (!item || !isAction(item)) throw new ExchangeProjectionError(`Unknown action: ${id}`);
	const items = [...exchange.items];
	items[index] = update(item);
	return { ...exchange, items };
}

function updateToolCall(
	exchange: Exchange,
	actionId: string,
	toolCallId: string,
	update: (action: ActionItem, toolCall: ToolCallExecution) => ToolCallExecution,
): Exchange {
	return updateAction(exchange, actionId, (action) => {
		const index = action.toolCalls.findIndex((toolCall) => toolCall.id === toolCallId);
		const toolCall = action.toolCalls[index];
		if (!toolCall) throw new ExchangeProjectionError(`Unknown tool call: ${toolCallId}`);
		const toolCalls = [...action.toolCalls];
		toolCalls[index] = update(action, toolCall);
		return { ...action, toolCalls };
	});
}

function assertUniqueToolCall(exchange: Exchange, toolCallId: string): void {
	if (exchange.items.some((item) => isAction(item) && item.toolCalls.some((toolCall) => toolCall.id === toolCallId))) {
		throw new ExchangeProjectionError(`Tool call already exists: ${toolCallId}`);
	}
}

function assertActionAndToolCallActive(action: ActionItem, toolCall: ToolCallExecution): void {
	if (action.status !== "in_progress") {
		throw new ExchangeProjectionError(`Action is not in progress: ${action.id}`);
	}
	if (toolCall.status !== "in_progress") {
		throw new ExchangeProjectionError(`Tool call is not in progress: ${toolCall.id}`);
	}
}

function findActiveToolCallForTurn(exchange: Exchange, modelTurnId: string): ToolCallExecution | undefined {
	for (const item of exchange.items) {
		if (!isAction(item)) continue;
		const toolCall = item.toolCalls.find(
			(candidate) => candidate.modelTurnId === modelTurnId && candidate.status === "in_progress",
		);
		if (toolCall) return toolCall;
	}
	return undefined;
}
