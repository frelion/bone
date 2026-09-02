import type { AgentMessage } from "@frelion/bone-agent-core";
import { type AssistantMessage, getTextContentPhase, type TextContent, type ToolResultMessage } from "@frelion/bone-ai";
import type { CustomMessage } from "./messages.ts";
import { SEMANTIC_ACTION_TOOL_NAME } from "./tools/semantic-action.ts";

export const AGENT_PROTOCOL_CORRECTION_CUSTOM_TYPE = "agent-protocol-correction";
export const AGENT_PROTOCOL_ERROR_KIND = "agent_protocol_error";

export type AgentProtocolErrorCode =
	| "ACTION_REQUIRED"
	| "STAGE_UPDATE_REQUIRED"
	| "STAGE_ACTION_REQUIRED"
	| "ACTION_DECLARATION_MUST_BE_EXCLUSIVE"
	| "MULTIPLE_ACTION_DECLARATIONS"
	| "FINAL_ANSWER_WITH_TOOL_CALLS"
	| "INVALID_ACTION_TITLE";

export interface AgentProtocolErrorDetails {
	internal: {
		kind: typeof AGENT_PROTOCOL_ERROR_KIND;
		code: AgentProtocolErrorCode;
		attempt: number;
		maxAttempts: number;
	};
}

export interface AgentProtocolViolation {
	code: AgentProtocolErrorCode;
	message: string;
}

export type AssistantResponseDisposition = "continuation" | "final" | "rejected";

export interface AgentProtocolResponse {
	disposition: AssistantResponseDisposition;
	commentary: string;
	finalAnswer: string;
	violation?: AgentProtocolViolation;
}

export function isAgentProtocolResponse(value: unknown): value is AgentProtocolResponse {
	if (!value || typeof value !== "object") return false;
	const response = value as Record<string, unknown>;
	if (
		response.disposition !== "continuation" &&
		response.disposition !== "final" &&
		response.disposition !== "rejected"
	) {
		return false;
	}
	if (typeof response.commentary !== "string" || typeof response.finalAnswer !== "string") return false;
	if (response.violation === undefined) return true;
	if (!response.violation || typeof response.violation !== "object") return false;
	const violation = response.violation as Record<string, unknown>;
	return typeof violation.code === "string" && typeof violation.message === "string";
}

export interface ClassifyAgentProtocolOptions {
	hasActiveAction: boolean;
	hasPendingSteering?: boolean;
}

export function validateAgentProtocolResponse(
	message: AssistantMessage,
	hasActiveAction: boolean,
): AgentProtocolViolation | undefined {
	return classifyAgentProtocolResponse(message, { hasActiveAction }).violation;
}

export function classifyAgentProtocolResponse(
	message: AssistantMessage,
	options: ClassifyAgentProtocolOptions,
): AgentProtocolResponse {
	const toolCalls = message.content.filter((part) => part.type === "toolCall");
	const actionCalls = toolCalls.filter((call) => call.name === SEMANTIC_ACTION_TOOL_NAME);
	const ordinaryCalls = toolCalls.filter((call) => call.name !== SEMANTIC_ACTION_TOOL_NAME);
	const textParts = message.content.filter(
		(part): part is TextContent => part.type === "text" && part.text.trim().length > 0,
	);
	const phasedText = textParts.map((part) => ({ text: part.text, phase: getTextContentPhase(part) }));
	const hasExplicitFinalAnswer = phasedText.some(({ phase }) => phase === "final_answer");
	const commentary = phasedText.filter(
		({ phase }) => phase === "commentary" || (phase === undefined && toolCalls.length > 0),
	);
	const rawCommentary = commentary.map(({ text }) => text).join("\n");
	const rawFinalAnswer = phasedText
		.filter(({ phase }) => phase === "final_answer" || (phase === undefined && toolCalls.length === 0))
		.map(({ text }) => text)
		.join("\n");
	let protocolViolation: AgentProtocolViolation | undefined;

	if (actionCalls.length > 1) {
		protocolViolation = violation(
			"MULTIPLE_ACTION_DECLARATIONS",
			"Declare exactly one Action per response. Call set_action once, then wait for the next model turn before doing work.",
		);
	} else if (actionCalls.length === 1 && ordinaryCalls.length > 0) {
		protocolViolation = violation(
			"ACTION_DECLARATION_MUST_BE_EXCLUSIVE",
			"set_action must be the only tool call in its response. Declare the Action now, then call ordinary tools in the next response.",
		);
	} else if (hasExplicitFinalAnswer && toolCalls.length > 0) {
		protocolViolation = violation(
			"FINAL_ANSWER_WITH_TOOL_CALLS",
			"A final answer cannot contain tool calls. Either provide the final answer with no tools, or continue work using the Action protocol.",
		);
	} else if (actionCalls.length === 1) {
		const title = actionCalls[0]?.arguments?.title;
		if (typeof title !== "string" || title.trim().length === 0 || title.trim().length > 120) {
			protocolViolation = violation(
				"INVALID_ACTION_TITLE",
				"set_action.title must be a non-empty user-facing objective no longer than 120 characters.",
			);
		}
	}
	if (!protocolViolation && commentary.length > 0) {
		if (actionCalls.length === 0) {
			protocolViolation = violation(
				"STAGE_ACTION_REQUIRED",
				"A stage update must include exactly one set_action call in the same response and no ordinary tools.",
			);
		}
	} else if (!protocolViolation && actionCalls.length === 1) {
		if (!options.hasActiveAction) {
			protocolViolation = violation(
				"STAGE_UPDATE_REQUIRED",
				"There is no active Action. Start the stage with normal commentary and one set_action call in the same response.",
			);
		}
	} else if (!protocolViolation && ordinaryCalls.length > 0 && !options.hasActiveAction) {
		protocolViolation = violation(
			"ACTION_REQUIRED",
			"Ordinary tools require an active Action. Start with a stage update plus one set_action call, then retry the tools in a later response.",
		);
	}

	const disposition: AssistantResponseDisposition = protocolViolation
		? "rejected"
		: message.stopReason === "stop" && !options.hasPendingSteering && toolCalls.length === 0 && rawFinalAnswer.trim()
			? "final"
			: "continuation";
	const reroutedFinal = disposition === "final" ? "" : rawFinalAnswer;
	return {
		disposition,
		commentary: [rawCommentary, reroutedFinal].filter(Boolean).join("\n"),
		finalAnswer: disposition === "final" ? rawFinalAnswer : "",
		...(protocolViolation ? { violation: protocolViolation } : {}),
	};
}

export function createAgentProtocolErrorDetails(
	code: AgentProtocolErrorCode,
	attempt: number,
	maxAttempts: number,
): AgentProtocolErrorDetails {
	return {
		internal: { kind: AGENT_PROTOCOL_ERROR_KIND, code, attempt, maxAttempts },
	};
}

export function createAgentProtocolCorrectionMessage(
	violation: AgentProtocolViolation,
	attempt: number,
	maxAttempts: number,
): CustomMessage<AgentProtocolErrorDetails> {
	return {
		role: "custom",
		customType: AGENT_PROTOCOL_CORRECTION_CUSTOM_TYPE,
		content: violation.message,
		display: false,
		details: createAgentProtocolErrorDetails(violation.code, attempt, maxAttempts),
		timestamp: Date.now(),
	};
}

export function isAgentProtocolToolResult(
	message: AgentMessage,
): message is ToolResultMessage<AgentProtocolErrorDetails> {
	return message.role === "toolResult" && isAgentProtocolErrorDetails(message.details);
}

export function isAgentProtocolCorrectionMessage(message: AgentMessage): boolean {
	return message.role === "custom" && message.customType === AGENT_PROTOCOL_CORRECTION_CUSTOM_TYPE;
}

export function isAgentProtocolErrorDetails(details: unknown): details is AgentProtocolErrorDetails {
	if (!details || typeof details !== "object") return false;
	const internal = (details as { internal?: unknown }).internal;
	return (
		internal !== null &&
		typeof internal === "object" &&
		(internal as { kind?: unknown }).kind === AGENT_PROTOCOL_ERROR_KIND
	);
}

function violation(code: AgentProtocolErrorCode, guidance: string): AgentProtocolViolation {
	return {
		code,
		message: `Agent protocol error [${code}]: ${guidance}`,
	};
}
