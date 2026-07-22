import { beforeEach, describe, expect, it, vi } from "vitest";
import type { QuestionRequest } from "../src/core/question.ts";
import { QuestionnaireComponent } from "../src/modes/interactive/components/questionnaire.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const LEFT = "\x1b[D";
const RIGHT = "\x1b[C";
const DOWN = "\x1b[B";

const request: QuestionRequest = {
	id: "request-1",
	toolCallId: "tool-1",
	createdAt: "now",
	questions: [
		{
			question: "Which mode?",
			header: "Mode",
			options: [
				{ label: "Fast", description: "Fast path" },
				{ label: "Safe", description: "Safe path" },
			],
		},
		{
			question: "Which clients?",
			header: "Clients",
			multiSelect: true,
			options: [
				{ label: "TUI", description: "Terminal" },
				{ label: "RPC", description: "Remote" },
			],
		},
	],
};

describe("QuestionnaireComponent", () => {
	beforeEach(() => initTheme("dark"));

	function rendered(component: QuestionnaireComponent): string {
		return component
			.render(100)
			.join("\n")
			.replace(/\u001b\[[0-9;]*m/g, "");
	}

	it("renders questions and Submit as left/right navigable tabs", () => {
		const component = new QuestionnaireComponent(request, vi.fn());
		expect(rendered(component)).toContain("□ Mode");
		expect(rendered(component)).toContain("□ Clients");
		expect(rendered(component)).toContain("✓ Submit");

		component.handleInput(RIGHT);
		expect(rendered(component)).toContain("Which clients?");
		component.handleInput(RIGHT);
		expect(rendered(component)).toContain("Review your answers");
		component.handleInput(LEFT);
		expect(rendered(component)).toContain("Which clients?");
	});

	it("selects and unselects a single option with Space without changing tabs", () => {
		const component = new QuestionnaireComponent(request, vi.fn());
		component.handleInput(" ");
		expect(rendered(component)).toContain("(*) Fast");
		expect(rendered(component)).toContain("Which mode?");

		component.handleInput(" ");
		expect(rendered(component)).toContain("( ) Fast");
		expect(rendered(component)).toContain("□ Mode");
	});

	it("toggles multiple options with Space and remains on the question tab", () => {
		const component = new QuestionnaireComponent(request, vi.fn());
		component.handleInput(RIGHT);
		component.handleInput(" ");
		component.handleInput(DOWN);
		component.handleInput(" ");

		expect(rendered(component)).toContain("[x] TUI");
		expect(rendered(component)).toContain("[x] RPC");
		expect(rendered(component)).toContain("Which clients?");
	});

	it("renders Other as a direct input and stores typed text without Enter", () => {
		const component = new QuestionnaireComponent(request, vi.fn());
		component.handleInput(DOWN);
		component.handleInput(DOWN);
		component.handleInput("Custom j/k mode");

		expect(rendered(component)).toContain("Custom j/k mode");
		expect(rendered(component)).toContain("■ Mode");
		expect(rendered(component)).toContain("Which mode?");
	});

	it("submits complete answers from the Submit tab", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput(" ");
		component.handleInput(RIGHT);
		component.handleInput(" ");
		component.handleInput(RIGHT);

		expect(rendered(component)).toContain("Review your answers");
		expect(rendered(component)).toContain("Mode: Fast");
		expect(rendered(component)).toContain("Clients: TUI");
		expect(done).not.toHaveBeenCalled();

		component.handleInput("\r");
		expect(done).toHaveBeenCalledWith({
			cancelled: false,
			answers: [
				{ questionIndex: 0, question: "Which mode?", kind: "option", answer: "Fast" },
				{ questionIndex: 1, question: "Which clients?", kind: "multi", answer: null, selected: ["TUI"] },
			],
		});
	});

	it("returns to the first unanswered question when Submit is incomplete", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput(RIGHT);
		component.handleInput(RIGHT);
		component.handleInput("\r");

		expect(done).not.toHaveBeenCalled();
		expect(rendered(component)).toContain("Which mode?");
		expect(rendered(component)).toContain("Answer every question before submitting.");
	});

	it("returns to Submit when cancellation is not confirmed", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput(LEFT);
		expect(rendered(component)).toContain("Review your answers");
		component.handleInput("\x1b");
		component.handleInput("n");

		expect(rendered(component)).toContain("Review your answers");
		expect(done).not.toHaveBeenCalled();
	});

	it("requires cancellation confirmation", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput("\x1b");
		expect(done).not.toHaveBeenCalled();
		component.handleInput("y");
		expect(done).toHaveBeenCalledWith({ cancelled: true });
	});
});
