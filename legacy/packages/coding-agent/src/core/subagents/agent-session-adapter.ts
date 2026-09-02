import type { SubagentHandoff, SubagentSession } from "@frelion/bone-agent-core";
import { type AssistantMessage, getTextContentPhase } from "@frelion/bone-ai";
import type { AgentSession } from "../agent-session.ts";

function handoffFromMessage(message: AssistantMessage): SubagentHandoff {
	const summary =
		message.content
			.filter(
				(part) =>
					part.type === "text" &&
					(getTextContentPhase(part) === "final_answer" || getTextContentPhase(part) === undefined),
			)
			.map((part) => (part.type === "text" ? part.text : ""))
			.join("\n")
			.trim() ||
		message.errorMessage ||
		"Subagent returned no summary";
	return {
		status: message.stopReason === "error" ? "failed" : message.stopReason === "stop" ? "completed" : "partial",
		summary,
	};
}

export function createAgentSessionSubagentSession(session: AgentSession): SubagentSession {
	return {
		run: async (input) => {
			const startIndex = session.messages.length;
			await session.prompt(input.text, { expandPromptTemplates: false, source: "extension" });
			const response = session.messages
				.slice(startIndex)
				.reverse()
				.find((message): message is AssistantMessage => message.role === "assistant");
			if (!response) throw new Error("Subagent session completed without an assistant response");
			return handoffFromMessage(response);
		},
		abort: async () => await session.abort(),
		close: async () => session.dispose(),
	};
}
