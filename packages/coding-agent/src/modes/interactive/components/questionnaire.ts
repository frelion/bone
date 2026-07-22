import { Container, getKeybindings, Input, Spacer, Text } from "@frelion/bone-tui";
import type { QuestionAnswer, QuestionRequest } from "../../../core/question.ts";
import { theme } from "../theme/theme.ts";
import { DynamicBorder } from "./dynamic-border.ts";
import { rawKeyHint } from "./keybinding-hints.ts";

export type QuestionnaireResult = { cancelled: false; answers: QuestionAnswer[] } | { cancelled: true };

type DraftAnswer =
	| { kind: "option"; value: string }
	| { kind: "custom"; value: string }
	| { kind: "multi"; values: Set<string> };

type ActiveViewState = { kind: "question" } | { kind: "review"; selectedRow: 0 | 1 | 2 };
type ViewState = ActiveViewState | { kind: "cancelConfirm"; returnTo: ActiveViewState };

export class QuestionnaireComponent extends Container {
	private readonly request: QuestionRequest;
	private readonly done: (result: QuestionnaireResult) => void;
	private questionIndex = 0;
	private selectedRow = 0;
	private drafts = new Map<number, DraftAnswer>();
	private customInput?: Input;
	private error?: string;
	private state: ViewState = { kind: "question" };

	constructor(request: QuestionRequest, done: (result: QuestionnaireResult) => void) {
		super();
		this.request = request;
		this.done = done;
		this.rebuild();
	}

	private get question() {
		return this.request.questions[this.questionIndex]!;
	}

	private get isLastQuestion(): boolean {
		return this.questionIndex === this.request.questions.length - 1;
	}

	private getDraft(index = this.questionIndex): DraftAnswer | undefined {
		return this.drafts.get(index);
	}

	private isAnswered(index: number): boolean {
		const draft = this.getDraft(index);
		return draft !== undefined && (draft.kind !== "multi" || draft.values.size > 0);
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

		const visibleState = this.state.kind === "cancelConfirm" ? this.state.returnTo : this.state;
		if (visibleState.kind === "review") this.renderReview(visibleState);
		else this.renderQuestion();

		this.addChild(new Spacer(1));
		this.renderFooter();
		this.addChild(new Spacer(1));
		this.addChild(new DynamicBorder());
	}

	private renderTabs(): void {
		const tabs = this.request.questions.map((question, index) => {
			const answered = this.isAnswered(index);
			const marker = answered ? "■" : "□";
			const label = ` ${marker} ${question.header} `;
			return index === this.questionIndex
				? theme.bg("selectedBg", theme.fg("text", label))
				: theme.fg(answered ? "success" : "muted", label);
		});
		this.addChild(new Text(`← ${tabs.join(" ")} →`, 1, 0));
		this.addChild(new Spacer(1));
	}

	private renderQuestion(): void {
		this.renderTabs();
		const draft = this.getDraft();
		const mode = this.question.multiSelect ? "Choose one or more options" : "Choose one option";
		this.addChild(
			new Text(
				theme.fg(
					"accent",
					theme.bold(`${this.question.header}  ${this.questionIndex + 1}/${this.request.questions.length}`),
				),
				1,
				0,
			),
		);
		this.addChild(new Text(this.question.question, 1, 0));
		this.addChild(new Text(theme.fg("muted", mode), 1, 0));
		this.addChild(new Spacer(1));

		for (let index = 0; index < this.question.options.length; index++) {
			const option = this.question.options[index]!;
			const focused = this.selectedRow === index;
			const checked =
				draft?.kind === "multi"
					? draft.values.has(option.label)
					: draft?.kind === "option" && draft.value === option.label;
			const marker = this.question.multiSelect ? (checked ? "[x]" : "[ ]") : checked ? "(*)" : "( )";
			const prefix = focused ? "❯" : " ";
			this.addRow(`${prefix} ${marker} ${option.label}`, focused, checked ? "accent" : undefined);
			this.addChild(new Text(`    ${theme.fg("muted", option.description)}`, 1, 0));
		}

		const otherIndex = this.question.options.length;
		const otherFocused = this.selectedRow === otherIndex;
		const otherSelected = draft?.kind === "custom";
		this.addRow(`${otherFocused ? "❯" : " "} ${otherSelected ? "(*)" : "( )"} Other`, otherFocused, "muted");
		if (this.question.multiSelect) {
			const nextIndex = otherIndex + 1;
			this.addRow(`${this.selectedRow === nextIndex ? "❯" : " "} Next →`, this.selectedRow === nextIndex, "accent");
		}
		if (this.customInput) {
			this.addChild(new Spacer(1));
			this.addChild(new Text(theme.fg("accent", "Custom answer"), 1, 0));
			this.addChild(this.customInput);
		}
		if (this.error) this.addChild(new Text(theme.fg("error", this.error), 1, 0));
	}

	private renderReview(state: Extract<ActiveViewState, { kind: "review" }>): void {
		const allAnswered = this.hasAllAnswers();
		const tabs = this.request.questions.map((question, index) => {
			const label = ` ■ ${question.header} `;
			return theme.fg(index === this.questionIndex ? "accent" : "success", label);
		});
		this.addChild(new Text(`← ${tabs.join(" ")} →`, 1, 0));
		this.addChild(new Spacer(1));
		this.addChild(new Text(theme.fg("accent", theme.bold("Review your answers")), 1, 0));
		this.addChild(new Spacer(1));

		for (let index = 0; index < this.request.questions.length; index++) {
			const question = this.request.questions[index]!;
			const draft = this.getDraft(index);
			let answer = "Unanswered";
			if (draft?.kind === "option" || draft?.kind === "custom") answer = draft.value;
			if (draft?.kind === "multi") answer = draft.values.size > 0 ? [...draft.values].join(", ") : "Unanswered";
			this.addRow(`${draft ? "✓" : "⚠"} ${question.header}: ${answer}`, false, draft ? undefined : "warning");
		}
		if (!allAnswered) {
			this.addChild(new Spacer(1));
			this.addChild(new Text(theme.fg("warning", "Answer the remaining questions before submitting."), 1, 0));
		}
		this.addChild(new Spacer(1));
		this.addRow(
			`${state.selectedRow === 0 ? "❯" : " "} Submit answers`,
			state.selectedRow === 0,
			allAnswered ? "accent" : "muted",
		);
		this.addRow(`${state.selectedRow === 1 ? "❯" : " "} Back to questions`, state.selectedRow === 1);
		this.addRow(`${state.selectedRow === 2 ? "❯" : " "} Cancel`, state.selectedRow === 2, "muted");
	}

	private renderFooter(): void {
		const visibleState = this.state.kind === "cancelConfirm" ? this.state.returnTo : this.state;
		if (visibleState.kind === "review") {
			this.addChild(
				new Text(
					`${rawKeyHint("↑↓", "choose")}  ${rawKeyHint("Enter", "confirm")}  ${rawKeyHint("Esc", "cancel")}`,
					1,
					0,
				),
			);
		} else if (this.customInput) {
			this.addChild(new Text(`${rawKeyHint("Enter", "save answer")}  ${rawKeyHint("Esc", "back")}`, 1, 0));
		} else {
			this.addChild(
				new Text(
					`${rawKeyHint("↑↓", "navigate")}  ${rawKeyHint("Enter", "select/next")}  ${rawKeyHint("Space", "toggle")}  ${rawKeyHint("Tab", "switch")}  ${rawKeyHint("Esc", "cancel")}`,
					1,
					0,
				),
			);
		}
		if (this.state.kind === "cancelConfirm") {
			this.addChild(
				new Text(
					theme.fg("warning", "Cancel this questionnaire? Press y to confirm or any other key to return."),
					1,
					0,
				),
			);
		}
	}

	private hasAllAnswers(): boolean {
		return (
			this.drafts.size === this.request.questions.length &&
			[...this.drafts.values()].every((draft) => draft.kind !== "multi" || draft.values.size > 0)
		);
	}

	private firstUnanswered(): number {
		for (let index = 0; index < this.request.questions.length; index++) {
			const draft = this.getDraft(index);
			if (!draft || (draft.kind === "multi" && draft.values.size === 0)) return index;
		}
		return -1;
	}

	private advanceAfterAnswer(): void {
		this.customInput = undefined;
		this.error = undefined;
		if (!this.isLastQuestion) {
			this.questionIndex++;
			this.selectedRow = 0;
			this.rebuild();
			return;
		}
		const unanswered = this.firstUnanswered();
		if (unanswered >= 0) {
			this.questionIndex = unanswered;
			this.selectedRow = 0;
			this.rebuild();
			return;
		}
		this.state = { kind: "review", selectedRow: 0 };
		this.rebuild();
	}

	private selectCurrent(): void {
		this.error = undefined;
		const otherIndex = this.question.options.length;
		if (this.selectedRow === otherIndex) {
			this.customInput = new Input();
			const draft = this.getDraft();
			if (draft?.kind === "custom") this.customInput.setValue(draft.value);
			this.rebuild();
			return;
		}
		if (this.question.multiSelect && this.selectedRow === otherIndex + 1) {
			const draft = this.getDraft();
			if (!draft || (draft.kind === "multi" && draft.values.size === 0)) {
				this.error = "Choose at least one option before continuing.";
				this.rebuild();
				return;
			}
			this.advanceAfterAnswer();
			return;
		}
		const option = this.question.options[this.selectedRow];
		if (!option) return;
		if (this.question.multiSelect) {
			const existing = this.getDraft();
			const values = existing?.kind === "multi" ? new Set(existing.values) : new Set<string>();
			if (values.has(option.label)) values.delete(option.label);
			else values.add(option.label);
			this.drafts.set(this.questionIndex, { kind: "multi", values });
			this.rebuild();
			return;
		}
		this.drafts.set(this.questionIndex, { kind: "option", value: option.label });
		this.advanceAfterAnswer();
	}

	private moveQuestion(delta: number): void {
		this.customInput = undefined;
		this.error = undefined;
		this.questionIndex = (this.questionIndex + delta + this.request.questions.length) % this.request.questions.length;
		this.selectedRow = 0;
		this.rebuild();
	}

	private submitReview(): void {
		if (!this.hasAllAnswers()) {
			const unanswered = this.firstUnanswered();
			this.questionIndex = unanswered >= 0 ? unanswered : 0;
			this.state = { kind: "question" };
			this.selectedRow = 0;
			this.error = "Answer every question before submitting.";
			this.rebuild();
			return;
		}
		const answers: QuestionAnswer[] = [];
		for (let index = 0; index < this.request.questions.length; index++) {
			const question = this.request.questions[index]!;
			const draft = this.getDraft(index)!;
			if (draft.kind === "multi") {
				answers.push({
					questionIndex: index,
					question: question.question,
					kind: "multi",
					answer: null,
					selected: [...draft.values],
				});
			} else {
				answers.push({ questionIndex: index, question: question.question, kind: draft.kind, answer: draft.value });
			}
		}
		this.done({ cancelled: false, answers });
	}

	private handleReviewInput(data: string): void {
		if (this.state.kind !== "review") return;
		const kb = getKeybindings();
		if (kb.matches(data, "tui.select.up") || data === "k") {
			this.state = { kind: "review", selectedRow: ((this.state.selectedRow + 2) % 3) as 0 | 1 | 2 };
			this.rebuild();
		} else if (kb.matches(data, "tui.select.down") || data === "j") {
			this.state = { kind: "review", selectedRow: ((this.state.selectedRow + 1) % 3) as 0 | 1 | 2 };
			this.rebuild();
		} else if (kb.matches(data, "tui.select.confirm") || data === "\n" || data === "\r") {
			if (this.state.selectedRow === 0) this.submitReview();
			else if (this.state.selectedRow === 1) {
				this.state = { kind: "question" };
				this.questionIndex = this.request.questions.length - 1;
				this.selectedRow = 0;
				this.rebuild();
			} else this.requestCancel();
		} else if (kb.matches(data, "tui.select.cancel")) this.requestCancel();
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
		if (this.state.kind === "review") {
			this.handleReviewInput(data);
			return;
		}

		const kb = getKeybindings();
		if (this.customInput) {
			if (kb.matches(data, "tui.select.cancel")) {
				this.customInput = undefined;
				this.rebuild();
				return;
			}
			if (data === "\r" || data === "\n") {
				const value = this.customInput.getValue().trim();
				if (!value) {
					this.error = "Custom answer must not be empty.";
					this.rebuild();
					return;
				}
				this.drafts.set(this.questionIndex, { kind: "custom", value });
				this.advanceAfterAnswer();
				return;
			}
			this.customInput.handleInput(data);
			return;
		}
		if (kb.matches(data, "tui.select.cancel")) {
			this.requestCancel();
		} else if (kb.matches(data, "tui.select.up") || data === "k") {
			const max = this.question.options.length + (this.question.multiSelect ? 1 : 0);
			this.selectedRow = (this.selectedRow + max) % (max + 1);
			this.rebuild();
		} else if (kb.matches(data, "tui.select.down") || data === "j") {
			const max = this.question.options.length + (this.question.multiSelect ? 1 : 0);
			this.selectedRow = (this.selectedRow + 1) % (max + 1);
			this.rebuild();
		} else if (data === "\t" || data === "l") {
			this.moveQuestion(1);
		} else if (data === "\u001b[Z" || data === "h") {
			this.moveQuestion(-1);
		} else if (data === " " && this.question.multiSelect) {
			this.selectCurrent();
		} else if (data === "\r" || data === "\n") {
			this.selectCurrent();
		}
	}
}
