import type { SubagentYieldInput } from "@frelion/bone-agent-core";
import { type Static, Type } from "typebox";
import type { ToolDefinition } from "../extensions/types.ts";

const yieldToParentSchema = Type.Object(
	{
		kind: Type.Union(
			[Type.Literal("progress"), Type.Literal("finding"), Type.Literal("risk"), Type.Literal("proposal")],
			{ description: "The semantic kind of this parent-visible update" },
		),
		message: Type.String({
			minLength: 1,
			maxLength: 1_000,
			description: "A concise self-contained update for the parent agent",
		}),
		artifactRefs: Type.Optional(
			Type.Array(Type.String({ minLength: 1, maxLength: 250 }), {
				maxItems: 5,
				description: "Optional artifact references supporting this update",
			}),
		),
	},
	{ additionalProperties: false },
);

type YieldToParentParams = Static<typeof yieldToParentSchema>;

/** Child-only tool. Publishing is synchronous and does not end the child run. */
export function createYieldToParentToolDefinition(
	publishYield: (input: SubagentYieldInput) => void,
): ToolDefinition<typeof yieldToParentSchema, { published: true }> {
	return {
		name: "yield_to_parent",
		label: "Yield to Parent",
		description:
			"Publish a concise progress, finding, risk, or proposal to the parent agent without ending this child run. Continue working after publishing.",
		promptSnippet: "Publish a concise update to the parent while continuing this child run",
		promptGuidelines: [
			"Use yield_to_parent only for information useful before the final handoff. Publishing does not end your task; continue working afterward.",
		],
		parameters: yieldToParentSchema,
		executionMode: "parallel",
		execute: async (_toolCallId, params: YieldToParentParams) => {
			publishYield({
				kind: params.kind,
				message: params.message,
				...(params.artifactRefs ? { artifactRefs: params.artifactRefs } : {}),
			});
			return {
				content: [{ type: "text", text: "Update published to parent. Continue the delegated task." }],
				details: { published: true as const },
			};
		},
	};
}
