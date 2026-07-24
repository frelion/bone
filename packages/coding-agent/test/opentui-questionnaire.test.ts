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
	const setup = await createTestRenderer({ width: 84, height: 28, autoFocus: false, kittyKeyboard: true });
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

function key(name: string, modifiers: { ctrl?: boolean; shift?: boolean; meta?: boolean } = {}): KeyEvent {
	return new KeyEvent({
		name,
		ctrl: modifiers.ctrl ?? false,
		meta: modifiers.meta ?? false,
		shift: modifiers.shift ?? false,
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
	test("keeps all question drafts in one panel and submits them together", async () => {
		const setup = await createQuestionnaireRenderer();
		const done = vi.fn<(result: OpenTUIQuestionnaireResult) => void>();
		const questionnaire = new OpenTUIQuestionnaire(setup.renderer, request(), done);
		setup.renderer.root.add(questionnaire.root);
		questionnaire.focus();

		await setup.flush();
		let frame = setup.captureCharFrame();
		expect(frame).toContain("Agent needs your input");
		expect(frame).toContain("[1 Build]  2 Targets");
		expect(frame).toContain("Which build should run?");

		questionnaire.handleKey(key("enter"));
		questionnaire.handleKey(key("tab"));
		questionnaire.handleKey(key("enter"));
		questionnaire.handleKey(key("down"));
		questionnaire.handleKey(key("enter"));
		questionnaire.handleKey(key("s", { ctrl: true }));

		expect(done).toHaveBeenCalledWith({
			cancelled: false,
			answers: [
				{
					questionIndex: 0,
					question: "Which build should run?",
					kind: "option",
					answer: "Local",
				},
				{
					questionIndex: 1,
					question: "Which targets are required?",
					kind: "multi",
					answer: null,
					selected: ["Linux", "macOS"],
				},
			],
		});

		await setup.flush();
		frame = setup.captureCharFrame();
		expect(frame).toContain("[x] Linux");
		expect(frame).toContain("[x] macOS");
	});

	test("validates incomplete answers and keeps custom input inside the panel", async () => {
		const setup = await createQuestionnaireRenderer();
		const done = vi.fn<(result: OpenTUIQuestionnaireResult) => void>();
		const questionnaire = new OpenTUIQuestionnaire(setup.renderer, request(), done);
		setup.renderer.root.add(questionnaire.root);
		questionnaire.focus();

		questionnaire.handleKey(key("s", { ctrl: true }));
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Answer every question before submitting.");
		expect(done).not.toHaveBeenCalled();

		questionnaire.handleKey(key("down"));
		questionnaire.handleKey(key("down"));
		questionnaire.handleKey(key("enter"));
		expect(questionnaire.customAnswerActive).toBe(true);
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Enter apply · Shift+Enter newline · Esc back");
		await setup.mockInput.typeText("Use the release build");
		setup.mockInput.pressEnter();
		await setup.flush();
		expect(questionnaire.customAnswerActive).toBe(false);
		expect(setup.captureCharFrame()).toContain("(*) Custom answer");
	});

	test("clears custom-answer validation when returning to options", async () => {
		const setup = await createQuestionnaireRenderer();
		const questionnaire = new OpenTUIQuestionnaire(setup.renderer, request(), vi.fn());
		setup.renderer.root.add(questionnaire.root);
		questionnaire.handleKey(key("down"));
		questionnaire.handleKey(key("down"));
		questionnaire.handleKey(key("enter"));
		questionnaire.handleKey(key("s", { ctrl: true }));
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Custom answer must not be empty.");

		questionnaire.handleKey(key("escape"));
		await setup.flush();
		expect(setup.captureCharFrame()).not.toContain("Custom answer must not be empty.");
		expect(setup.captureCharFrame()).toContain("Enter choose · Tab next · Ctrl+S submit · Esc cancel");
	});

	test("cancels without answering when Escape is pressed", async () => {
		const setup = await createQuestionnaireRenderer();
		const done = vi.fn<(result: OpenTUIQuestionnaireResult) => void>();
		const questionnaire = new OpenTUIQuestionnaire(setup.renderer, request(), done);
		setup.renderer.root.add(questionnaire.root);

		questionnaire.handleKey(key("escape"));

		expect(done).toHaveBeenCalledWith({ cancelled: true });
	});

	test("restores partial drafts when a pending question surface is remounted", async () => {
		const setup = await createQuestionnaireRenderer();
		const first = new OpenTUIQuestionnaire(setup.renderer, request(), vi.fn());
		setup.renderer.root.add(first.root);
		first.handleKey(key("enter"));
		const draft = first.getDraftAnswers();
		first.root.destroyRecursively();

		const done = vi.fn<(result: OpenTUIQuestionnaireResult) => void>();
		const restored = new OpenTUIQuestionnaire(setup.renderer, request(), done, draft);
		setup.renderer.root.add(restored.root);
		restored.handleKey(key("tab"));
		restored.handleKey(key("enter"));
		restored.handleKey(key("s", { ctrl: true }));

		expect(done).toHaveBeenCalledWith({
			cancelled: false,
			answers: [
				{
					questionIndex: 0,
					question: "Which build should run?",
					kind: "option",
					answer: "Local",
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
});
