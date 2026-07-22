import { beforeEach, describe, expect, it, vi } from "vitest";
import type { QuestionRequest } from "../src/core/question.ts";
import { QuestionnaireComponent } from "../src/modes/interactive/components/questionnaire.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

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

	it("advances through questions and shows review before submission", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput("\r");
		expect(rendered(component)).toContain("Clients  2/2");
		component.handleInput(" ");
		component.handleInput("\x1b[A");
		component.handleInput("\r");

		expect(done).not.toHaveBeenCalled();
		expect(rendered(component)).toContain("Review your answers");
		expect(rendered(component)).toContain("Mode: Fast");
		expect(rendered(component)).toContain("Clients: TUI");

		component.handleInput("\r");

		expect(done).toHaveBeenCalledWith({
			cancelled: false,
			answers: [
				{ questionIndex: 0, question: "Which mode?", kind: "option", answer: "Fast" },
				{ questionIndex: 1, question: "Which clients?", kind: "multi", answer: null, selected: ["TUI"] },
			],
		});
	});

	it("returns to the first unanswered question after completing the last question", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput("\t");
		component.handleInput(" ");
		component.handleInput("\x1b[A");
		component.handleInput("\r");

		expect(done).not.toHaveBeenCalled();
		expect(rendered(component)).toContain("Mode  1/2");
		expect(rendered(component)).not.toContain("Review your answers");
	});

	it("allows returning from review to modify answers", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput("\r");
		component.handleInput(" ");
		component.handleInput("\x1b[A");
		component.handleInput("\r");
		component.handleInput("\x1b[B");
		component.handleInput("\r");

		expect(rendered(component)).toContain("Clients  2/2");
		expect(done).not.toHaveBeenCalled();
	});

	it("collects an Other answer and advances", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput("\x1b[A");
		component.handleInput("\r");
		component.handleInput("Custom mode");
		component.handleInput("\r");

		expect(rendered(component)).toContain("Clients  2/2");
		expect(rendered(component)).toContain("■ Mode");
	});

	it("does not confirm a single-select question with Space", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput(" ");

		expect(rendered(component)).toContain("Mode  1/2");
		expect(rendered(component)).not.toContain("■ Mode");
		expect(done).not.toHaveBeenCalled();
	});

	it("shows review after answering a single-question request", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent({ ...request, questions: [request.questions[0]!] }, done);
		component.handleInput("\r");

		expect(rendered(component)).toContain("Review your answers");
		expect(done).not.toHaveBeenCalled();
		component.handleInput("\r");
		expect(done).toHaveBeenCalledOnce();
	});

	it("returns to review when cancellation is not confirmed", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput("\r");
		component.handleInput(" ");
		component.handleInput("\x1b[A");
		component.handleInput("\r");
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
