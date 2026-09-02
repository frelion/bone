import { type Static, Type } from "typebox";
import { type AgentTool, AgentToolError, type AgentToolResult } from "../types.ts";
import type { SubagentHandle, SubagentHandoff, SubagentRuntime, SubagentYield } from "./types.ts";
import { SubagentRuntimeError } from "./types.ts";

const agentRefSchema = Type.String({
	minLength: 1,
	description: "Opaque agentRef returned by delegate_stage",
});

const delegateStageSchema = Type.Object(
	{
		objective: Type.String({
			minLength: 1,
			maxLength: 20_000,
			description: "Self-contained objective for the delegated work package",
		}),
		label: Type.Optional(
			Type.String({
				minLength: 1,
				maxLength: 120,
				description: "Short user-visible label for the child agent",
			}),
		),
		scope: Type.Optional(
			Type.Union([Type.Literal("exchange"), Type.Literal("conversation")], {
				description: "exchange closes with the parent exchange; conversation supports later questions",
			}),
		),
		contextRefs: Type.Optional(
			Type.Array(Type.String({ minLength: 1, maxLength: 2_000 }), {
				maxItems: 100,
				description: "Relevant file, symbol, or artifact references; do not inline large contents",
			}),
		),
		expectedOutput: Type.Optional(
			Type.String({
				minLength: 1,
				maxLength: 4_000,
				description: "Acceptance criteria and requested handoff shape",
			}),
		),
	},
	{ additionalProperties: false },
);

const askAgentSchema = Type.Object(
	{
		agentRef: agentRefSchema,
		question: Type.String({
			minLength: 1,
			maxLength: 20_000,
			description: "Follow-up question for the child agent's persistent context",
		}),
	},
	{ additionalProperties: false },
);

const waitAgentSchema = Type.Object({ agentRef: agentRefSchema }, { additionalProperties: false });

const cancelAgentSchema = Type.Object(
	{
		agentRef: agentRefSchema,
		reason: Type.Optional(Type.String({ minLength: 1, maxLength: 1_000 })),
	},
	{ additionalProperties: false },
);

const closeAgentSchema = Type.Object({ agentRef: agentRefSchema }, { additionalProperties: false });
const readAgentMessagesSchema = Type.Object({ agentRef: agentRefSchema }, { additionalProperties: false });

type DelegateStageParams = Static<typeof delegateStageSchema>;
type AskAgentParams = Static<typeof askAgentSchema>;
type WaitAgentParams = Static<typeof waitAgentSchema>;
type CancelAgentParams = Static<typeof cancelAgentSchema>;
type CloseAgentParams = Static<typeof closeAgentSchema>;
type ReadAgentMessagesParams = Static<typeof readAgentMessagesSchema>;

function toolResult<T>(value: T): AgentToolResult<T> {
	return {
		content: [{ type: "text", text: JSON.stringify(value) }],
		details: value,
	};
}

function mapRuntimeError(error: unknown): never {
	if (error instanceof SubagentRuntimeError) {
		throw new AgentToolError(`subagent_${error.code}`, error.message, error.code === "busy", {
			operation: "subagent",
		});
	}
	throw error;
}

export interface SubagentToolSet {
	delegateStage: AgentTool<typeof delegateStageSchema, SubagentHandle>;
	askAgent: AgentTool<typeof askAgentSchema, SubagentHandoff>;
	waitAgent: AgentTool<typeof waitAgentSchema, SubagentHandoff>;
	cancelAgent: AgentTool<typeof cancelAgentSchema, SubagentHandle>;
	closeAgent: AgentTool<typeof closeAgentSchema, { agentRef: string; closed: true }>;
	readAgentMessages: AgentTool<typeof readAgentMessagesSchema, SubagentYield[]>;
}

/**
 * Creates the model-facing control surface. Runtime progress remains on the
 * runtime event stream; tool results contain only handles or bounded handoffs.
 */
export function createSubagentTools(runtime: SubagentRuntime): SubagentToolSet {
	return {
		delegateStage: {
			name: "delegate_stage",
			label: "Delegate Stage",
			description:
				"Start a child agent for a coherent work stage. Returns immediately with an agentRef. Use wait_agent for its bounded handoff and ask_agent for later questions. Child tool events stay out of the parent model context.",
			parameters: delegateStageSchema,
			executionMode: "parallel",
			execute: async (_toolCallId, params: DelegateStageParams, signal) => {
				try {
					return toolResult(
						await runtime.delegate(
							{
								objective: params.objective,
								...(params.label ? { label: params.label } : {}),
								...(params.scope ? { scope: params.scope } : {}),
								...(params.contextRefs ? { contextRefs: params.contextRefs } : {}),
								...(params.expectedOutput ? { expectedOutput: params.expectedOutput } : {}),
							},
							signal,
						),
					);
				} catch (error) {
					mapRuntimeError(error);
				}
			},
		},
		askAgent: {
			name: "ask_agent",
			label: "Ask Agent",
			description:
				"Ask an idle child agent a follow-up question using its persistent private context. Returns only a bounded structured handoff.",
			parameters: askAgentSchema,
			executionMode: "parallel",
			execute: async (_toolCallId, params: AskAgentParams, signal) => {
				try {
					return toolResult(await runtime.ask(params.agentRef, params.question, signal));
				} catch (error) {
					mapRuntimeError(error);
				}
			},
		},
		waitAgent: {
			name: "wait_agent",
			label: "Wait for Agent",
			description:
				"Wait for a delegated child agent and return its bounded structured handoff. Raw child turns, logs, and tool events are not returned.",
			parameters: waitAgentSchema,
			executionMode: "parallel",
			execute: async (_toolCallId, params: WaitAgentParams, signal) => {
				try {
					return toolResult(await runtime.wait(params.agentRef, signal));
				} catch (error) {
					mapRuntimeError(error);
				}
			},
		},
		cancelAgent: {
			name: "cancel_agent",
			label: "Cancel Agent",
			description:
				"Cancel the child agent's active run while keeping its persistent session available for later questions.",
			parameters: cancelAgentSchema,
			executionMode: "parallel",
			execute: async (_toolCallId, params: CancelAgentParams) => {
				try {
					return toolResult(await runtime.cancel(params.agentRef, params.reason));
				} catch (error) {
					mapRuntimeError(error);
				}
			},
		},
		closeAgent: {
			name: "close_agent",
			label: "Close Agent",
			description: "Close a child-agent session when its private context is no longer needed.",
			parameters: closeAgentSchema,
			executionMode: "parallel",
			execute: async (_toolCallId, params: CloseAgentParams) => {
				try {
					await runtime.close(params.agentRef);
					return toolResult({ agentRef: params.agentRef, closed: true as const });
				} catch (error) {
					mapRuntimeError(error);
				}
			},
		},
		readAgentMessages: {
			name: "read_agent_messages",
			label: "Read Agent Messages",
			description:
				"Atomically read and clear bounded progress, findings, risks, and proposals published by a child agent.",
			parameters: readAgentMessagesSchema,
			executionMode: "parallel",
			execute: async (_toolCallId, params: ReadAgentMessagesParams, signal) => {
				try {
					if (signal?.aborted) {
						throw new SubagentRuntimeError(
							"cancelled",
							typeof signal.reason === "string" ? signal.reason : "Subagent operation was cancelled",
						);
					}
					return toolResult(runtime.drainYields(params.agentRef));
				} catch (error) {
					mapRuntimeError(error);
				}
			},
		},
	};
}
