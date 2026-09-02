export const PROPOSED_PLAN_OPEN_TAG = "<proposed_plan>";
export const PROPOSED_PLAN_CLOSE_TAG = "</proposed_plan>";

export type CollaborationMode = "default" | "plan";

export interface PlanProposal {
	id: string;
	version: number;
	content: string;
	createdAt: string;
	sourceMessageId: string;
}

export type PlanState =
	| { status: "inactive" }
	| { status: "planning" }
	| { status: "awaitingApproval"; proposal: PlanProposal };

export type PlanDecision = "approved" | "revision_requested" | "cancelled";

export type ProposedPlanParseResult =
	| { status: "none" }
	| { status: "valid"; content: string }
	| { status: "invalid"; error: string };

export const PLAN_MODE_TOOL_NAMES = [
	"read",
	"grep",
	"find",
	"ls",
	"ask_user_question",
	"forge_context",
	"forge_query",
	"forge_audit",
	"forge_watch",
] as const;

export const PLAN_MODE_INSTRUCTIONS = `# Plan Mode

You are in Plan Mode until the host application explicitly exits it. User wording, tone, or requests to start implementation do not change the active mode.

You may inspect and search the workspace, but you must not make changes or perform implementation work. Resolve facts that can be discovered from the workspace before asking the user. If the requested outcome, constraints, or acceptance criteria are ambiguous, use ask_user_question for material choices with concrete options; use a concise plain-text question only when options would not represent the decision fairly.

When the change is clear enough to approve, return exactly one complete proposal using this format:

<proposed_plan>
# Short title

## Summary
Briefly describe the intended result.

## Changes
Describe the behavioral and implementation changes.

## Verification
Describe how the result will be verified.

## Assumptions
List only material assumptions or risks, when present.
</proposed_plan>

The opening and closing tags must each be on their own line. Do not emit a proposal while material decisions remain unresolved. A revised proposal must be a complete replacement, not a patch to a previous proposal.`;

function countOccurrences(text: string, value: string): number {
	let count = 0;
	let offset = 0;
	while (true) {
		const index = text.indexOf(value, offset);
		if (index === -1) return count;
		count++;
		offset = index + value.length;
	}
}

export function parseProposedPlan(text: string): ProposedPlanParseResult {
	const openCount = countOccurrences(text, PROPOSED_PLAN_OPEN_TAG);
	const closeCount = countOccurrences(text, PROPOSED_PLAN_CLOSE_TAG);
	if (openCount === 0 && closeCount === 0) {
		return { status: "none" };
	}
	if (openCount !== 1 || closeCount !== 1) {
		return { status: "invalid", error: "A plan response must contain exactly one proposed_plan block." };
	}

	const pattern = /(?:^|\r?\n)<proposed_plan>[ \t]*\r?\n([\s\S]*?)\r?\n<\/proposed_plan>(?=\r?\n|$)/;
	const match = pattern.exec(text);
	if (!match) {
		return {
			status: "invalid",
			error: "The proposed_plan tags must be complete and appear on their own lines.",
		};
	}

	const content = match[1]?.trim();
	if (!content) {
		return { status: "invalid", error: "The proposed plan cannot be empty." };
	}
	return { status: "valid", content };
}

export function removeProposedPlanBlock(text: string): string {
	const openIndex = text.indexOf(PROPOSED_PLAN_OPEN_TAG);
	if (openIndex === -1) return text;
	const closeIndex = text.indexOf(PROPOSED_PLAN_CLOSE_TAG, openIndex + PROPOSED_PLAN_OPEN_TAG.length);
	if (closeIndex === -1) return text;
	return `${text.slice(0, openIndex)}${text.slice(closeIndex + PROPOSED_PLAN_CLOSE_TAG.length)}`.trim();
}

export function appendPlanModeInstructions(systemPrompt: string): string {
	return `${systemPrompt.trimEnd()}\n\n${PLAN_MODE_INSTRUCTIONS}`;
}
