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
	it("collects single and multi-select answers before submission", () => {
		const done = vi.fn();
		const component = new QuestionnaireComponent(request, done);
		component.handleInput("\r");
		component.handleInput("\t");
		component.handleInput(" ");
		component.handleInput("s");

		expect(done).toHaveBeenCalledWith({
			cancelled: false,
			answers: [
				{ questionIndex: 0, question: "Which mode?", kind: "option", answer: "Fast" },
				{ questionIndex: 1, question: "Which clients?", kind: "multi", answer: null, selected: ["TUI"] },
			],
		});
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
