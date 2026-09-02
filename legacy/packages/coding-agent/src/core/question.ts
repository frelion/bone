import type { AgentToolResult } from "@frelion/bone-agent-core";
import { type AskUserQuestionInput, askUserQuestionSchema, type QuestionToolDetails } from "@frelion/bone-session";
import type { ToolDefinition } from "./extensions/types.ts";

export * from "@frelion/bone-session";

export function createAskUserQuestionToolDefinition(
	executeQuestion: (
		toolCallId: string,
		input: AskUserQuestionInput,
		signal?: AbortSignal,
	) => Promise<AgentToolResult<QuestionToolDetails>>,
): ToolDefinition<typeof askUserQuestionSchema, QuestionToolDetails> {
	return {
		name: "ask_user_question",
		label: "Ask User Question",
		description:
			"Ask the user one to four structured questions when a material product decision cannot be discovered from the workspace. Each question must offer two to four concrete options and may include a Markdown preview for choices that benefit from visual detail. The user may select options, add a note to each answer, add overall notes, or cancel.",
		promptSnippet: "Ask the user structured questions when a material decision cannot be discovered",
		promptGuidelines: [
			"Investigate discoverable workspace facts before using ask_user_question. Use it only for material user preferences, requirements, constraints, or acceptance criteria.",
			"Group related decisions into one invocation. Do not ask the user to choose internal helper names, file locations, or test organization.",
			"Put the recommended option first and explain each option's trade-off in its description.",
			"Use an option preview for concise code, Markdown, ASCII diagrams, or concrete output examples when that detail materially helps the user decide.",
		],
		parameters: askUserQuestionSchema,
		executionMode: "sequential",
		execute: async (toolCallId, input, signal) => executeQuestion(toolCallId, input, signal),
	};
}
