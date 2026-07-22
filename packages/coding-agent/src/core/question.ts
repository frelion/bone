import type { AgentToolResult } from "@frelion/bone-agent-core";
import { type Static, Type } from "typebox";
import type { ToolDefinition } from "./extensions/types.ts";

export const MIN_QUESTION_OPTIONS = 2;
export const MAX_QUESTION_OPTIONS = 4;
export const MAX_QUESTIONS = 4;
export const MAX_QUESTION_HEADER_LENGTH = 16;
export const RESERVED_QUESTION_OPTION_LABELS = new Set(["other", "cancel"]);

export interface QuestionOption {
	label: string;
	description: string;
}

export interface QuestionDefinition {
	question: string;
	header: string;
	options: QuestionOption[];
	multiSelect?: boolean;
}

export interface QuestionRequest {
	id: string;
	toolCallId: string;
	questions: QuestionDefinition[];
	createdAt: string;
}

export interface QuestionAnswer {
	questionIndex: number;
	question: string;
	kind: "option" | "custom" | "multi";
	answer: string | null;
	selected?: string[];
}

export type QuestionCancelReason = "user" | "abort" | "client_disconnect" | "no_ui";

export type QuestionState = { status: "inactive" } | { status: "awaitingAnswer"; request: QuestionRequest };

export interface QuestionToolDetails {
	requestId?: string;
	answers: QuestionAnswer[];
	cancelled: boolean;
	reason?: QuestionCancelReason;
}

const questionOptionSchema = Type.Object({
	label: Type.String({ minLength: 1, maxLength: 60 }),
	description: Type.String({ minLength: 1 }),
});

export const askUserQuestionSchema = Type.Object({
	questions: Type.Array(
		Type.Object({
			question: Type.String({ minLength: 1 }),
			header: Type.String({ minLength: 1, maxLength: MAX_QUESTION_HEADER_LENGTH }),
			options: Type.Array(questionOptionSchema, {
				minItems: MIN_QUESTION_OPTIONS,
				maxItems: MAX_QUESTION_OPTIONS,
			}),
			multiSelect: Type.Optional(Type.Boolean()),
		}),
		{ minItems: 1, maxItems: MAX_QUESTIONS },
	),
});

export type AskUserQuestionInput = Static<typeof askUserQuestionSchema>;

function normalized(value: string): string {
	return value.trim().toLocaleLowerCase();
}

export function validateQuestionDefinitions(input: AskUserQuestionInput): QuestionDefinition[] {
	return input.questions.map((question, questionIndex) => {
		const text = question.question.trim();
		const header = question.header.trim();
		if (!text) throw new Error(`Question ${questionIndex + 1} must not be empty.`);
		if (!header) throw new Error(`Question ${questionIndex + 1} header must not be empty.`);
		if (header.length > MAX_QUESTION_HEADER_LENGTH) {
			throw new Error(
				`Question ${questionIndex + 1} header must be at most ${MAX_QUESTION_HEADER_LENGTH} characters.`,
			);
		}

		const labels = new Set<string>();
		const options = question.options.map((option, optionIndex) => {
			const label = option.label.trim();
			const description = option.description.trim();
			if (!label || !description) {
				throw new Error(
					`Question ${questionIndex + 1} option ${optionIndex + 1} must have a label and description.`,
				);
			}
			const key = normalized(label);
			if (RESERVED_QUESTION_OPTION_LABELS.has(key)) {
				throw new Error(`Question ${questionIndex + 1} option label "${label}" is reserved.`);
			}
			if (labels.has(key)) throw new Error(`Question ${questionIndex + 1} has duplicate option label "${label}".`);
			labels.add(key);
			return { label, description };
		});

		return { question: text, header, options, ...(question.multiSelect && { multiSelect: true }) };
	});
}

export function validateQuestionAnswers(request: QuestionRequest, answers: QuestionAnswer[]): QuestionAnswer[] {
	if (answers.length !== request.questions.length)
		throw new Error("Every question must be answered before submitting.");
	const byIndex = new Map<number, QuestionAnswer>();
	for (const answer of answers) {
		if (
			!Number.isInteger(answer.questionIndex) ||
			answer.questionIndex < 0 ||
			answer.questionIndex >= request.questions.length
		) {
			throw new Error(`Invalid question index ${answer.questionIndex}.`);
		}
		if (byIndex.has(answer.questionIndex))
			throw new Error(`Question ${answer.questionIndex + 1} was answered more than once.`);
		byIndex.set(answer.questionIndex, answer);
	}

	return request.questions.map((question, questionIndex) => {
		const answer = byIndex.get(questionIndex);
		if (!answer) throw new Error(`Question ${questionIndex + 1} is unanswered.`);
		if (answer.question !== question.question)
			throw new Error(`Question ${questionIndex + 1} text does not match the request.`);

		const optionLabels = new Set(question.options.map((option) => option.label));
		if (question.multiSelect) {
			if (answer.kind === "custom") {
				const value = answer.answer?.trim();
				if (!value) throw new Error(`Question ${questionIndex + 1} custom answer must not be empty.`);
				return { questionIndex, question: question.question, kind: "custom" as const, answer: value };
			}
			if (answer.kind !== "multi" || !answer.selected?.length) {
				throw new Error(`Question ${questionIndex + 1} requires at least one selected option.`);
			}
			const selected = [...new Set(answer.selected)];
			if (selected.some((label) => !optionLabels.has(label))) {
				throw new Error(`Question ${questionIndex + 1} contains an unknown option.`);
			}
			return { questionIndex, question: question.question, kind: "multi" as const, answer: null, selected };
		}

		if (answer.kind === "option") {
			if (!answer.answer || !optionLabels.has(answer.answer)) {
				throw new Error(`Question ${questionIndex + 1} must select a valid option.`);
			}
			return { questionIndex, question: question.question, kind: "option" as const, answer: answer.answer };
		}
		if (answer.kind === "custom") {
			const value = answer.answer?.trim();
			if (!value) throw new Error(`Question ${questionIndex + 1} custom answer must not be empty.`);
			return { questionIndex, question: question.question, kind: "custom" as const, answer: value };
		}
		throw new Error(`Question ${questionIndex + 1} does not accept multiple selections.`);
	});
}

export function createQuestionToolResult(
	requestId: string,
	answers: QuestionAnswer[],
): AgentToolResult<QuestionToolDetails> {
	return {
		content: [{ type: "text", text: JSON.stringify({ requestId, answers }, null, 2) }],
		details: { requestId, answers, cancelled: false },
	};
}

export function createCancelledQuestionToolResult(
	requestId: string,
	reason: QuestionCancelReason,
): AgentToolResult<QuestionToolDetails> {
	return {
		content: [{ type: "text", text: `The user cancelled the questionnaire (${reason}).` }],
		details: { requestId, answers: [], cancelled: true, reason },
	};
}

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
			"Ask the user one to four structured questions when a material product decision cannot be discovered from the workspace. Each question must offer two to four concrete options. The user can also provide a custom answer or cancel.",
		promptSnippet: "Ask the user structured questions when a material decision cannot be discovered",
		promptGuidelines: [
			"Investigate discoverable workspace facts before using ask_user_question. Use it only for material user preferences, requirements, constraints, or acceptance criteria.",
			"Group related decisions into one invocation. Do not ask the user to choose internal helper names, file locations, or test organization.",
			"Put the recommended option first and explain each option's trade-off in its description.",
		],
		parameters: askUserQuestionSchema,
		executionMode: "sequential",
		execute: async (toolCallId, input, signal) => executeQuestion(toolCallId, input, signal),
	};
}
