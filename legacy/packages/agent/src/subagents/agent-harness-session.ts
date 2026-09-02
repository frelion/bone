import type { AssistantMessage, ImageContent } from "@frelion/bone-ai";
import type { SubagentHandoff, SubagentRunInput, SubagentSession } from "./types.ts";

export interface AgentHarnessSubagentAdapter {
	prompt(text: string, options?: { images?: ImageContent[] }): Promise<AssistantMessage>;
	abort(): Promise<unknown>;
}

export type AgentHarnessHandoffMapper = (
	message: AssistantMessage,
	input: SubagentRunInput,
) => SubagentHandoff | Promise<SubagentHandoff>;

function assistantText(message: AssistantMessage): string {
	return message.content
		.flatMap((part) => (part.type === "text" ? [part.text] : []))
		.join("")
		.trim();
}

function defaultHandoff(message: AssistantMessage): SubagentHandoff {
	const summary = assistantText(message) || message.errorMessage || "Subagent returned no summary";
	return {
		status: message.stopReason === "error" ? "failed" : message.stopReason === "stop" ? "completed" : "partial",
		summary,
	};
}

/** Adapts a persistent AgentHarness instance to the transport-neutral child-session contract. */
export function createAgentHarnessSubagentSession(
	harness: AgentHarnessSubagentAdapter,
	mapHandoff: AgentHarnessHandoffMapper = defaultHandoff,
): SubagentSession {
	return {
		run: async (input) => await mapHandoff(await harness.prompt(input.text), input),
		abort: async () => {
			await harness.abort();
		},
	};
}
