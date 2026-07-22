import { Container, getKeybindings, Input, Key, matchesKey, Spacer, Text } from "@frelion/bone-tui";
import type { QuestionAnswer, QuestionRequest } from "../../../core/question.ts";
import { theme } from "../theme/theme.ts";
import { DynamicBorder } from "./dynamic-border.ts";
import { rawKeyHint } from "./keybinding-hints.ts";

export type QuestionnaireResult = { cancelled: false; answers: QuestionAnswer[] } | { cancelled: true };

type DraftAnswer =
	| { kind: "option"; value: string }
	| { kind: "custom"; value: string }
	| { kind: "multi"; values: Set<string> };

type ActiveViewState =
	| { kind: "question"; questionIndex: number; selectedRow: number }
	| { kind: "submit"; selectedRow: 0 | 1 };
type ViewState = ActiveViewState | { kind: "cancelConfirm"; returnTo: ActiveViewState };

export class QuestionnaireComponent extends Container {
	private readonly request: QuestionRequest;
	private readonly done: (result: QuestionnaireResult) => void;
	private readonly drafts = new Map<number, DraftAnswer>();
	private readonly customInputs = new Map<number, Input>();
	private state: ViewState = { kind: "question", questionIndex: 0, selectedRow: 0 };
	private error?: string;

	constructor(request: QuestionRequest, done: (result: QuestionnaireResult) => void) {
		super();
		this.request = request;
		this.done = done;
		this.rebuild();
	}

	private get activeState(): ActiveViewState {
		return this.state.kind === "cancelConfirm" ? this.state.returnTo : this.state;
	}

	private get currentTab(): number {
		const state = this.activeState;
		return state.kind === "submit" ? this.request.questions.length : state.questionIndex;
	}

	private getDraft(index: number): DraftAnswer | undefined {
		return this.drafts.get(index);
	}

	private getCustomInput(index: number): Input {
		let input = this.customInputs.get(index);
		if (!input) {
			input = new Input();
			this.customInputs.set(index, input);
		}
		return input;
	}

	private isAnswered(index: number): boolean {
		const draft = this.getDraft(index);
		return draft !== undefined && (draft.kind !== "multi" || draft.values.size > 0);
	}

	private hasAllAnswers(): boolean {
		return this.request.questions.every((_, index) => this.isAnswered(index));
	}

	private firstUnanswered(): number {
		return this.request.questions.findIndex((_, index) => !this.isAnswered(index));
	}

	private addRow(text: string, selected = false, color?: "accent" | "muted" | "warning" | "error"): void {
		const styled = color ? theme.fg(color, text) : text;
		this.addChild(
			new Text(
				selected ? theme.fg("text", styled) : styled,
				1,
				0,
				selected ? (line) => theme.bg("selectedBg", line) : undefined,
			),
		);
	}

	private rebuild(): void {
		this.clear();
		this.addChild(new DynamicBorder());
		this.addChild(new Spacer(1));
		this.renderTabs();
		this.addChild(new Spacer(1));

		const state = this.activeState;
		if (state.kind === "submit") this.renderSubmit(state);
		else this.renderQuestion(state);

		this.addChild(new Spacer(1));
		this.renderFooter(state);
		if (this.state.kind === "cancelConfirm") {
			this.addChild(
				new Text(
					theme.fg("warning", "Cancel this questionnaire? Press y to confirm or any other key to return."),
					1,
					0,
				),
			);
		}
		this.addChild(new Spacer(1));
		this.addChild(new DynamicBorder());
	}

	private renderTabs(): void {
		const tabs = this.request.questions.map((question, index) => {
			const answered = this.isAnswered(index);
			return this.renderTab(` ${answered ? "■" : "□"} ${question.header} `, index, answered ? "success" : "muted");
		});
		const submitColor = this.hasAllAnswers() ? "success" : "dim";
		tabs.push(this.renderTab(" ✓ Submit ", this.request.questions.length, submitColor));
		this.addChild(new Text(`← ${tabs.join(" ")} →`, 1, 0));
	}

	private renderTab(label: string, index: number, color: "success" | "muted" | "dim"): string {
		return this.currentTab === index ? theme.bg("selectedBg", theme.fg("text", label)) : theme.fg(color, label);
	}

	private renderQuestion(state: Extract<ActiveViewState, { kind: "question" }>): void {
		const question = this.request.questions[state.questionIndex]!;
		const draft = this.getDraft(state.questionIndex);
		const mode = question.multiSelect ? "Choose one or more options" : "Choose one option";
		this.addChild(new Text(theme.fg("accent", theme.bold(question.header)), 1, 0));
		this.addChild(new Text(question.question, 1, 0));
		this.addChild(new Text(theme.fg("muted", mode), 1, 0));
		this.addChild(new Spacer(1));

		for (let index = 0; index < question.options.length; index++) {
			const option = question.options[index]!;
			const focused = state.selectedRow === index;
			const checked =
				draft?.kind === "multi"
					? draft.values.has(option.label)
					: draft?.kind === "option" && draft.value === option.label;
			const marker = question.multiSelect ? (checked ? "[x]" : "[ ]") : checked ? "(*)" : "( )";
			this.addRow(`${focused ? "❯" : " "} ${marker} ${option.label}`, focused, checked ? "accent" : undefined);
			this.addChild(new Text(`    ${theme.fg("muted", option.description)}`, 1, 0));
		}

		const inputFocused = state.selectedRow === question.options.length;
		const input = this.getCustomInput(state.questionIndex);
		input.focused = inputFocused;
		this.addRow(`${inputFocused ? "❯" : " "} Other`, inputFocused, "muted");
		this.addChild(input);
		if (this.error) this.addChild(new Text(theme.fg("error", this.error), 1, 0));
	}

	private renderSubmit(state: Extract<ActiveViewState, { kind: "submit" }>): void {
		this.addChild(new Text(theme.fg("accent", theme.bold("Review your answers")), 1, 0));
		this.addChild(new Spacer(1));
		for (let index = 0; index < this.request.questions.length; index++) {
			const question = this.request.questions[index]!;
			const draft = this.getDraft(index);
			let answer = "Unanswered";
			if (draft?.kind === "option" || draft?.kind === "custom") answer = draft.value;
			if (draft?.kind === "multi") answer = [...draft.values].join(", ") || "Unanswered";
			this.addRow(
				`${this.isAnswered(index) ? "✓" : "⚠"} ${question.header}: ${answer}`,
				false,
				this.isAnswered(index) ? undefined : "warning",
			);
		}
		if (!this.hasAllAnswers()) {
			this.addChild(new Spacer(1));
			this.addChild(new Text(theme.fg("warning", "Answer the remaining questions before submitting."), 1, 0));
		}
		this.addChild(new Spacer(1));
		this.addRow(
			`${state.selectedRow === 0 ? "❯" : " "} Submit answers`,
			state.selectedRow === 0,
			this.hasAllAnswers() ? "accent" : "muted",
		);
		this.addRow(`${state.selectedRow === 1 ? "❯" : " "} Cancel`, state.selectedRow === 1, "muted");
	}

	private renderFooter(state: ActiveViewState): void {
		const hints =
			state.kind === "submit"
				? `${rawKeyHint("←→", "switch tab")}  ${rawKeyHint("↑↓", "choose")}  ${rawKeyHint("Enter", "confirm")}  ${rawKeyHint("Esc", "cancel")}`
				: `${rawKeyHint("←→", "switch tab")}  ${rawKeyHint("↑↓", "navigate")}  ${rawKeyHint("Space", "select/unselect")}  ${rawKeyHint("Esc", "cancel")}`;
		this.addChild(new Text(hints, 1, 0));
	}

	private moveTab(delta: number): void {
		const total = this.request.questions.length + 1;
		const next = (this.currentTab + delta + total) % total;
		this.error = undefined;
		this.state =
			next === this.request.questions.length
				? { kind: "submit", selectedRow: 0 }
				: { kind: "question", questionIndex: next, selectedRow: 0 };
		this.rebuild();
	}

	private moveRow(delta: number): void {
		const state = this.activeState;
		if (state.kind === "submit") {
			this.state = { kind: "submit", selectedRow: state.selectedRow === 0 ? 1 : 0 };
		} else {
			const optionCount = this.request.questions[state.questionIndex]!.options.length + 1;
			this.state = {
				...state,
				selectedRow: (state.selectedRow + delta + optionCount) % optionCount,
			};
		}
		this.rebuild();
	}

	private toggleSelectedOption(): void {
		const state = this.activeState;
		if (state.kind !== "question") return;
		const question = this.request.questions[state.questionIndex]!;
		if (state.selectedRow === question.options.length) {
			this.forwardToCustomInput(" ");
			return;
		}
		const option = question.options[state.selectedRow];
		if (!option) return;
		const existing = this.getDraft(state.questionIndex);
		if (question.multiSelect) {
			const values = existing?.kind === "multi" ? new Set(existing.values) : new Set<string>();
			if (values.has(option.label)) values.delete(option.label);
			else values.add(option.label);
			if (values.size === 0) this.drafts.delete(state.questionIndex);
			else this.drafts.set(state.questionIndex, { kind: "multi", values });
		} else if (existing?.kind === "option" && existing.value === option.label) {
			this.drafts.delete(state.questionIndex);
		} else {
			this.drafts.set(state.questionIndex, { kind: "option", value: option.label });
		}
		this.getCustomInput(state.questionIndex).setValue("");
		this.error = undefined;
		this.rebuild();
	}

	private forwardToCustomInput(data: string): void {
		const state = this.activeState;
		if (state.kind !== "question") return;
		const input = this.getCustomInput(state.questionIndex);
		input.handleInput(data);
		const value = input.getValue().trim();
		if (value) this.drafts.set(state.questionIndex, { kind: "custom", value });
		else this.drafts.delete(state.questionIndex);
		this.error = undefined;
		this.rebuild();
	}

	private submitAnswers(): void {
		if (!this.hasAllAnswers()) {
			const unanswered = this.firstUnanswered();
			this.state = { kind: "question", questionIndex: unanswered >= 0 ? unanswered : 0, selectedRow: 0 };
			this.error = "Answer every question before submitting.";
			this.rebuild();
			return;
		}
		const answers = this.request.questions.map<QuestionAnswer>((question, index) => {
			const draft = this.getDraft(index)!;
			return draft.kind === "multi"
				? {
						questionIndex: index,
						question: question.question,
						kind: "multi",
						answer: null,
						selected: [...draft.values],
					}
				: { questionIndex: index, question: question.question, kind: draft.kind, answer: draft.value };
		});
		this.done({ cancelled: false, answers });
	}

	private requestCancel(): void {
		if (this.state.kind === "cancelConfirm") return;
		this.state = { kind: "cancelConfirm", returnTo: this.state };
		this.rebuild();
	}

	handleInput(data: string): void {
		if (this.state.kind === "cancelConfirm") {
			if (data.toLocaleLowerCase() === "y") this.done({ cancelled: true });
			else {
				this.state = this.state.returnTo;
				this.rebuild();
			}
			return;
		}
		if (matchesKey(data, Key.left)) {
			this.moveTab(-1);
			return;
		}
		if (matchesKey(data, Key.right)) {
			this.moveTab(1);
			return;
		}

		const kb = getKeybindings();
		if (kb.matches(data, "tui.select.cancel")) {
			this.requestCancel();
			return;
		}
		if (kb.matches(data, "tui.select.up")) {
			this.moveRow(-1);
			return;
		}
		if (kb.matches(data, "tui.select.down")) {
			this.moveRow(1);
			return;
		}

		const state = this.activeState;
		if (state.kind === "submit") {
			if (kb.matches(data, "tui.select.confirm") || data === "\n" || data === "\r") {
				if (state.selectedRow === 0) this.submitAnswers();
				else this.requestCancel();
			}
			return;
		}
		const question = this.request.questions[state.questionIndex]!;
		if (state.selectedRow === question.options.length) {
			this.forwardToCustomInput(data);
		} else if (matchesKey(data, Key.space)) {
			this.toggleSelectedOption();
		}
	}
}
