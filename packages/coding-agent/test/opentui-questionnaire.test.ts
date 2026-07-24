import { KeyEvent } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { QuestionRequest } from "../src/core/question.ts";
import {
	OpenTUIQuestionnaire,
	type OpenTUIQuestionnaireResult,
} from "../src/modes/interactive/components/opentui-questionnaire.ts";

const renderers = new Set<TestRendererSetup>();

async function createQuestionnaireRenderer() {
	const setup = await createTestRenderer({ width: 100, height: 32, autoFocus: false, kittyKeyboard: true });
	renderers.add(setup);
	return setup;
}

function request(): QuestionRequest {
	return {
		id: "request-1",
		toolCallId: "call-1",
		createdAt: new Date(0).toISOString(),
		questions: [
			{
				header: "Build",
				question: "Which build should run?",
				options: [
					{ label: "Local", description: "Only local development" },
					{ label: "CI", description: "Local development and CI", preview: "bun run check" },
				],
			},
			{
				header: "Targets",
				question: "Which targets are required?",
				multiSelect: true,
				options: [
					{ label: "Linux", description: "Linux runner" },
					{ label: "macOS", description: "macOS runner" },
				],
			},
		],
	};
}

function key(name: string): KeyEvent {
	return new KeyEvent({
		name,
		ctrl: false,
		meta: false,
		shift: false,
		option: false,
		sequence: "",
		number: false,
		raw: "",
		eventType: "press",
		source: "raw",
	});
}

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUIQuestionnaire", () => {
	test("keeps option choices and per-question notes, then submits from Review", async () => {
		const setup = await createQuestionnaireRenderer();
		const done = vi.fn<(result: OpenTUIQuestionnaireResult) => void>();
		const questionnaire = new OpenTUIQuestionnaire(setup.renderer, request(), done);
		setup.renderer.root.add(questionnaire.root);
		questionnaire.focus();

		questionnaire.handleKey(key("enter"));
		questionnaire.handleKey(key("down"));
		questionnaire.handleKey(key("down"));
		expect(questionnaire.questionNoteActive).toBe(true);
		await setup.mockInput.typeText("Prefer the fast local path");
		setup.mockInput.pressEnter();
		questionnaire.handleKey(key("right"));
		questionnaire.handleKey(key("enter"));
		questionnaire.handleKey(key("right"));
		expect(questionnaire.reviewActive).toBe(true);
		questionnaire.handleKey(key("down"));
		questionnaire.handleKey(key("enter"));
		await setup.mockInput.typeText("Ship both decisions together");
		setup.mockInput.pressEnter();
		questionnaire.handleKey(key("up"));
		questionnaire.handleKey(key("enter"));

		expect(done).toHaveBeenCalledWith({
			cancelled: false,
			answers: [
				{
					questionIndex: 0,
					question: "Which build should run?",
					kind: "option",
					answer: "Local",
					notes: "Prefer the fast local path",
				},
				{
					questionIndex: 1,
					question: "Which targets are required?",
					kind: "multi",
					answer: null,
					selected: ["Linux"],
				},
			],
			overallNotes: "Ship both decisions together",
		});
	});

	test("accepts a note without an option and reports the first unanswered question", async () => {
		const setup = await createQuestionnaireRenderer();
		const done = vi.fn<(result: OpenTUIQuestionnaireResult) => void>();
		const questionnaire = new OpenTUIQuestionnaire(setup.renderer, request(), done);
		setup.renderer.root.add(questionnaire.root);

		questionnaire.handleKey(key("right"));
		questionnaire.handleKey(key("right"));
		questionnaire.handleKey(key("enter"));
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Answer question 1");
		expect(done).not.toHaveBeenCalled();

		questionnaire.handleKey(key("down"));
		questionnaire.handleKey(key("down"));
		await setup.mockInput.typeText("Use a remote build farm instead");
		setup.mockInput.pressEnter();
		questionnaire.handleKey(key("right"));
		questionnaire.handleKey(key("enter"));
		questionnaire.handleKey(key("right"));
		questionnaire.handleKey(key("enter"));

		expect(done).toHaveBeenCalledWith({
			cancelled: false,
			answers: [
				{
					questionIndex: 0,
					question: "Which build should run?",
					kind: "note",
					answer: null,
					notes: "Use a remote build farm instead",
				},
				{
					questionIndex: 1,
					question: "Which targets are required?",
					kind: "multi",
					answer: null,
					selected: ["Linux"],
				},
			],
		});
	});

	test("restores selections and both note levels after remounting", async () => {
		const setup = await createQuestionnaireRenderer();
		const first = new OpenTUIQuestionnaire(setup.renderer, request(), vi.fn(), {
			answers: [
				{
					questionIndex: 0,
					question: "Which build should run?",
					kind: "option",
					answer: "CI",
					notes: "Keep logs",
				},
			],
			overallNotes: "Release context",
		});
		setup.renderer.root.add(first.root);
		await setup.flush();
		const frame = setup.captureCharFrame();
		expect(frame).toContain("(*) CI");
		expect(frame).toContain("Keep logs");
		expect(first.getDraft()).toEqual({
			answers: [
				{
					questionIndex: 0,
					question: "Which build should run?",
					kind: "option",
					answer: "CI",
					notes: "Keep logs",
				},
			],
			overallNotes: "Release context",
		});
	});

	test("cancels from the option surface", () => {
		const setupPromise = createQuestionnaireRenderer();
		return setupPromise.then((setup) => {
			const done = vi.fn<(result: OpenTUIQuestionnaireResult) => void>();
			const questionnaire = new OpenTUIQuestionnaire(setup.renderer, request(), done);
			questionnaire.handleKey(key("escape"));
			expect(done).toHaveBeenCalledWith({ cancelled: true });
		});
	});
});
