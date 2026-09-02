import {
	AgentToolError,
	createSubagentTools,
	type SubagentHandle,
	type SubagentHandoff,
	type SubagentYield,
} from "@frelion/bone-agent-core";
import type { ToolDefinition } from "../extensions/types.ts";
import type { CodingSubagentManager, SubagentOrigin } from "./types.ts";

export const SUBAGENT_TOOL_NAMES = [
	"delegate_stage",
	"ask_agent",
	"wait_agent",
	"cancel_agent",
	"close_agent",
	"read_agent_messages",
] as const;

export type SubagentToolName = (typeof SUBAGENT_TOOL_NAMES)[number];

export interface CreateSubagentToolDefinitionsOptions {
	manager: CodingSubagentManager;
	resolveOrigin(toolCallId: string): SubagentOrigin | undefined;
}

export function createSubagentToolDefinitions(
	options: CreateSubagentToolDefinitionsOptions,
): Record<SubagentToolName, ToolDefinition> {
	const tools = createSubagentTools(options.manager.runtime);
	const delegateStage: ToolDefinition<typeof tools.delegateStage.parameters, SubagentHandle> = {
		...tools.delegateStage,
		promptSnippet: "Delegate a coherent work stage to an isolated child agent",
		promptGuidelines: [
			"Use delegate_stage for independent work that would add substantial file, log, or diagnostic noise to the parent context.",
			"After delegating, use wait_agent before relying on the result. Use ask_agent for later questions against the child agent's retained context.",
			"Only bounded handoffs enter the parent context; do not ask the child to return raw logs or full file contents.",
		],
		execute: async (toolCallId, params, signal, onUpdate) => {
			const origin = options.resolveOrigin(toolCallId);
			if (!origin) {
				throw new AgentToolError(
					"subagent_origin_missing",
					"delegate_stage must run inside an active Action",
					false,
				);
			}
			const result = await tools.delegateStage.execute(toolCallId, params, signal, onUpdate);
			options.manager.register(result.details, origin);
			return result;
		},
		renderV2: {
			summarize: ({ args }) => `Delegate ${args.label?.trim() || args.objective.trim()}`,
		},
	};
	const askAgent: ToolDefinition<typeof tools.askAgent.parameters, SubagentHandoff> = {
		...tools.askAgent,
		promptSnippet: "Ask a retained child agent a focused follow-up question",
		execute: async (toolCallId, params, signal, onUpdate) => {
			const result = await tools.askAgent.execute(toolCallId, params, signal, onUpdate);
			options.manager.recordHandoff(params.agentRef, result.details);
			return result;
		},
		renderV2: { summarize: ({ args }) => `Ask agent ${args.agentRef}` },
	};
	const waitAgent: ToolDefinition<typeof tools.waitAgent.parameters, SubagentHandoff> = {
		...tools.waitAgent,
		promptSnippet: "Wait for a child agent's bounded handoff",
		execute: async (toolCallId, params, signal, onUpdate) => {
			const result = await tools.waitAgent.execute(toolCallId, params, signal, onUpdate);
			options.manager.recordHandoff(params.agentRef, result.details);
			return result;
		},
		renderV2: { summarize: ({ args }) => `Wait for agent ${args.agentRef}` },
	};
	const cancelAgent: ToolDefinition<typeof tools.cancelAgent.parameters, SubagentHandle> = {
		...tools.cancelAgent,
		promptSnippet: "Cancel a child agent's active run",
		renderV2: { summarize: ({ args }) => `Cancel agent ${args.agentRef}` },
	};
	const closeAgent: ToolDefinition<typeof tools.closeAgent.parameters, { agentRef: string; closed: true }> = {
		...tools.closeAgent,
		promptSnippet: "Close a retained child-agent session",
		renderV2: { summarize: ({ args }) => `Close agent ${args.agentRef}` },
	};
	const readAgentMessages: ToolDefinition<typeof tools.readAgentMessages.parameters, SubagentYield[]> = {
		...tools.readAgentMessages,
		promptSnippet: "Consume unread messages explicitly yielded by a delegated child agent",
		promptGuidelines: [
			"Use read_agent_messages when an active child may have useful interim findings. It consumes only explicitly yielded messages; use wait_agent for the final handoff.",
		],
		execute: async (toolCallId, params, signal, onUpdate) => {
			const result = await tools.readAgentMessages.execute(toolCallId, params, signal, onUpdate);
			options.manager.recordYieldsRead(params.agentRef, result.details);
			return result;
		},
		renderV2: { summarize: ({ args }) => `Read messages from agent ${args.agentRef}` },
	};
	return {
		delegate_stage: delegateStage,
		ask_agent: askAgent,
		wait_agent: waitAgent,
		cancel_agent: cancelAgent,
		close_agent: closeAgent,
		read_agent_messages: readAgentMessages,
	};
}
