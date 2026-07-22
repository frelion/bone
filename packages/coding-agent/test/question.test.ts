import { describe, expect, it } from "vitest";
import {
	type AskUserQuestionInput,
	type QuestionRequest,
	validateQuestionAnswers,
	validateQuestionDefinitions,
} from "../src/core/question.ts";

const input: AskUserQuestionInput = {
	questions: [
		{
			question: "Which API should we expose?",
			header: "API",
			options: [
				{ label: "Minimal", description: "Expose only the stable core." },
				{ label: "Complete", description: "Expose every operation." },
			],
		},
		{
			question: "Which clients need support?",
			header: "Clients",
			multiSelect: true,
			options: [
				{ label: "TUI", description: "Interactive terminal client." },
				{ label: "RPC", description: "Programmatic clients." },
			],
		},
	],
};

function request(): QuestionRequest {
	return { id: "request-1", toolCallId: "tool-1", questions: validateQuestionDefinitions(input), createdAt: "now" };
}

describe("structured questions", () => {
	it("normalizes valid single-select and multi-select questions", () => {
		expect(validateQuestionDefinitions(input)).toEqual(input.questions);
	});

	it("normalizes optional Markdown previews", () => {
		const withPreview: AskUserQuestionInput = {
			questions: [
				{
					...input.questions[0],
					options: [
						{ ...input.questions[0].options[0], preview: "  ```ts\nconst safe = true;\n```  " },
						input.questions[0].options[1],
					],
				},
			],
		};

		expect(validateQuestionDefinitions(withPreview)[0]?.options[0]?.preview).toBe("```ts\nconst safe = true;\n```");
	});

	it("rejects duplicate and reserved labels", () => {
		expect(() =>
			validateQuestionDefinitions({
				questions: [
					{
						...input.questions[0],
						options: [
							{ label: "Other", description: "x" },
							{ label: "OK", description: "y" },
						],
					},
				],
			}),
		).toThrow("reserved");
		expect(() =>
			validateQuestionDefinitions({
				questions: [
					{
						...input.questions[0],
						options: [
							{ label: "Same", description: "x" },
							{ label: "same", description: "y" },
						],
					},
				],
			}),
		).toThrow("duplicate");
	});

	it("validates a complete answer set", () => {
		expect(
			validateQuestionAnswers(request(), [
				{
					questionIndex: 0,
					question: input.questions[0].question,
					kind: "custom",
					answer: "A smaller public facade",
				},
				{
					questionIndex: 1,
					question: input.questions[1].question,
					kind: "multi",
					answer: null,
					selected: ["RPC", "TUI", "RPC"],
				},
			]),
		).toEqual([
			{ questionIndex: 0, question: input.questions[0].question, kind: "custom", answer: "A smaller public facade" },
			{
				questionIndex: 1,
				question: input.questions[1].question,
				kind: "multi",
				answer: null,
				selected: ["RPC", "TUI"],
			},
		]);
	});

	it("normalizes supplemental notes for selected answers", () => {
		expect(
			validateQuestionAnswers(request(), [
				{
					questionIndex: 0,
					question: input.questions[0].question,
					kind: "option",
					answer: "Minimal",
					notes: "  Keep the facade small  ",
				},
				{
					questionIndex: 1,
					question: input.questions[1].question,
					kind: "multi",
					answer: null,
					selected: ["TUI"],
					notes: "  RPC can follow later  ",
				},
			]),
		).toEqual([
			{
				questionIndex: 0,
				question: input.questions[0].question,
				kind: "option",
				answer: "Minimal",
				notes: "Keep the facade small",
			},
			{
				questionIndex: 1,
				question: input.questions[1].question,
				kind: "multi",
				answer: null,
				selected: ["TUI"],
				notes: "RPC can follow later",
			},
		]);
	});

	it("allows a custom answer for a multi-select question", () => {
		expect(
			validateQuestionAnswers(request(), [
				{ questionIndex: 0, question: input.questions[0].question, kind: "option", answer: "Minimal" },
				{ questionIndex: 1, question: input.questions[1].question, kind: "custom", answer: "A browser client" },
			]),
		).toEqual([
			{ questionIndex: 0, question: input.questions[0].question, kind: "option", answer: "Minimal" },
			{ questionIndex: 1, question: input.questions[1].question, kind: "custom", answer: "A browser client" },
		]);
	});

	it("rejects incomplete, unknown, duplicate, and empty answers", () => {
		expect(() => validateQuestionAnswers(request(), [])).toThrow("Every question");
		expect(() =>
			validateQuestionAnswers(request(), [
				{ questionIndex: 0, question: input.questions[0].question, kind: "option", answer: "Unknown" },
				{ questionIndex: 1, question: input.questions[1].question, kind: "multi", answer: null, selected: ["TUI"] },
			]),
		).toThrow("valid option");
		expect(() =>
			validateQuestionAnswers(request(), [
				{ questionIndex: 0, question: input.questions[0].question, kind: "custom", answer: " " },
				{ questionIndex: 1, question: input.questions[1].question, kind: "multi", answer: null, selected: [] },
			]),
		).toThrow("custom answer");
	});
});
