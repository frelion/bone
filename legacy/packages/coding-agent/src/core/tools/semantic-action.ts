import type { AgentToolResult } from "@frelion/bone-agent-core";
import { type Static, Type } from "typebox";
import type { ToolDefinition } from "../extensions/types.ts";

export const SEMANTIC_ACTION_TOOL_NAME = "set_action";

export const semanticActionSchema = Type.Object({
	title: Type.String({
		minLength: 1,
		maxLength: 120,
		description: "Short user-facing description of the concrete objective now in progress",
	}),
});

export type SemanticActionInput = Static<typeof semanticActionSchema>;

export function createSemanticActionToolDefinition(
	setAction: (toolCallId: string, title: string) => void,
): ToolDefinition<typeof semanticActionSchema> {
	return {
		name: SEMANTIC_ACTION_TOOL_NAME,
		label: "Set Action",
		description:
			"Declare the concrete user-visible objective currently in progress. At the start of a coarse-grained stage, write one normal commentary update and call set_action as the only tool call in that same response. For later Actions in the same stage, call set_action directly without commentary. Inspection, design, editing, targeted tests, and fixes are normally Actions in one stage. Run ordinary tools only in following model turns. One Action may span multiple tools and model turns; do not write prose merely to announce the next Action. A protocol error means the whole requested tool batch was rejected and must be corrected.",
		parameters: semanticActionSchema,
		executionMode: "sequential",
		execute: async (toolCallId, input): Promise<AgentToolResult<never>> => {
			const title = input.title.trim();
			if (!title) throw new Error("Action title must not be empty");
			setAction(toolCallId, title);
			return { content: [{ type: "text", text: "Action active." }], details: undefined as never };
		},
	};
}
