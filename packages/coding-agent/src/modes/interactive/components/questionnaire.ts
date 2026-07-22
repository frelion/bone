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

export class QuestionnaireComponent extends Container {
	private request: QuestionRequest;
	private done: (result: QuestionnaireResult) => void;
	private questionIndex = 0;
	private selectedRow = 0;
	private drafts = new Map<number, DraftAnswer>();
	private customInput?: Input;
	private confirmingCancel = false;
	private error?: string;

	constructor(request: QuestionRequest, done: (result: QuestionnaireResult) => void) {
		super();
		this.request = request;
		this.done = done;
		this.rebuild();
	}

	private get question() {
		return this.request.questions[this.questionIndex];
	}

	private rebuild(): void {
		this.clear();
		this.addChild(new DynamicBorder());
		this.addChild(new Spacer(1));
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
		this.addChild(new Spacer(1));

		const draft = this.drafts.get(this.questionIndex);
		for (let index = 0; index < this.question.options.length; index++) {
			const option = this.question.options[index];
			const focused = index === this.selectedRow;
			const checked =
				draft?.kind === "multi"
					? draft.values.has(option.label)
					: draft?.kind === "option" && draft.value === option.label;
			const marker = this.question.multiSelect ? (checked ? "[x]" : "[ ]") : checked ? "(*)" : "( )";
			const line = `${focused ? ">" : " "} ${marker} ${option.label} - ${option.description}`;
			this.addChild(new Text(focused ? theme.fg("accent", line) : line, 1, 0));
		}

		const otherRow = this.question.options.length;
		const otherFocused = this.selectedRow === otherRow;
		const otherSelected = draft?.kind === "custom";
		this.addChild(new Text(`${otherFocused ? ">" : " "} ${otherSelected ? "(*)" : "( )"} Other`, 1, 0));
		if (this.customInput) {
			this.addChild(new Spacer(1));
			this.addChild(this.customInput);
		}

		this.addChild(new Spacer(1));
		if (this.error) this.addChild(new Text(theme.fg("error", this.error), 1, 0));
		if (this.confirmingCancel) {
			this.addChild(
				new Text(
					theme.fg("warning", "Cancel this questionnaire? Press y to confirm or any other key to return."),
					1,
					0,
				),
			);
		} else if (this.customInput) {
			this.addChild(new Text(`${rawKeyHint("Enter", "save Other")}  ${rawKeyHint("Esc", "back")}`, 1, 0));
		} else {
			this.addChild(
				new Text(
					rawKeyHint("↑↓", "navigate") +
						"  " +
						rawKeyHint("Space/Enter", this.question.multiSelect ? "toggle" : "select") +
						"  " +
						rawKeyHint("Tab", "next") +
						"  " +
						rawKeyHint("s", "submit") +
						"  " +
						rawKeyHint("Esc", "cancel"),
					1,
					0,
				),
			);
		}
		this.addChild(new Spacer(1));
		this.addChild(new DynamicBorder());
	}

	private selectCurrent(): void {
		this.error = undefined;
		if (this.selectedRow === this.question.options.length) {
			this.customInput = new Input();
			const draft = this.drafts.get(this.questionIndex);
			if (draft?.kind === "custom") this.customInput.setValue(draft.value);
			this.rebuild();
			return;
		}
		const option = this.question.options[this.selectedRow];
		if (this.question.multiSelect) {
			const existing = this.drafts.get(this.questionIndex);
			const values = existing?.kind === "multi" ? new Set(existing.values) : new Set<string>();
			if (values.has(option.label)) values.delete(option.label);
			else values.add(option.label);
			this.drafts.set(this.questionIndex, { kind: "multi", values });
		} else {
			this.drafts.set(this.questionIndex, { kind: "option", value: option.label });
		}
		this.rebuild();
	}

	private moveQuestion(delta: number): void {
		this.questionIndex = (this.questionIndex + delta + this.request.questions.length) % this.request.questions.length;
		this.selectedRow = 0;
		this.error = undefined;
		this.rebuild();
	}

	private submit(): void {
		const answers: QuestionAnswer[] = [];
		for (let index = 0; index < this.request.questions.length; index++) {
			const question = this.request.questions[index];
			const draft = this.drafts.get(index);
			if (!draft || (draft.kind === "multi" && draft.values.size === 0)) {
				this.questionIndex = index;
				this.selectedRow = 0;
				this.error = "Answer every question before submitting.";
				this.rebuild();
				return;
			}
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

	handleInput(data: string): void {
		if (this.confirmingCancel) {
			if (data.toLocaleLowerCase() === "y") this.done({ cancelled: true });
			else {
				this.confirmingCancel = false;
				this.rebuild();
			}
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
				this.customInput = undefined;
				this.rebuild();
				return;
			}
			this.customInput.handleInput(data);
			return;
		}

		if (kb.matches(data, "tui.select.cancel")) {
			this.confirmingCancel = true;
			this.rebuild();
		} else if (kb.matches(data, "tui.select.up") || data === "k") {
			this.selectedRow = Math.max(0, this.selectedRow - 1);
			this.rebuild();
		} else if (kb.matches(data, "tui.select.down") || data === "j") {
			this.selectedRow = Math.min(this.question.options.length, this.selectedRow + 1);
			this.rebuild();
		} else if (data === "\t" || data === "l") {
			this.moveQuestion(1);
		} else if (data === "h") {
			this.moveQuestion(-1);
		} else if (data === " " || data === "\r" || data === "\n") {
			this.selectCurrent();
		} else if (data.toLocaleLowerCase() === "s") {
			this.submit();
		}
	}
}
