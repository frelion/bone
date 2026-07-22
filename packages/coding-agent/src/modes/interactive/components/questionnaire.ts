import {
	type Component,
	Container,
	getKeybindings,
	Input,
	Key,
	Markdown,
	matchesKey,
	Spacer,
	Text,
	truncateToWidth,
	visibleWidth,
} from "@frelion/bone-tui";
import type { QuestionAnswer, QuestionRequest } from "../../../core/question.ts";
import { getMarkdownTheme, theme } from "../theme/theme.ts";
import { DynamicBorder } from "./dynamic-border.ts";
import { rawKeyHint } from "./keybinding-hints.ts";
import { fitLine } from "./terminal-layout.ts";

export type QuestionnaireResult = { cancelled: false; answers: QuestionAnswer[] } | { cancelled: true };

interface DraftAnswer {
	option?: string;
	selected?: Set<string>;
	custom?: string;
}

type ActiveViewState = { kind: "question"; questionIndex: number; selectedRow: number } | { kind: "submit" };
type ViewState = ActiveViewState | { kind: "cancelConfirm"; returnTo: ActiveViewState };

const DETAILS_SIDE_BY_SIDE_MIN_WIDTH = 100;
const DETAILS_MIN_LEFT_WIDTH = 30;
const DETAILS_MIN_RIGHT_WIDTH = 45;

class QuestionChoicePane implements Component {
	private readonly question: QuestionRequest["questions"][number];
	private readonly selectedRow: number;
	private readonly draft: DraftAnswer | undefined;
	private readonly input: Input;
	private readonly allQuestions: QuestionRequest["questions"];

	constructor(
		question: QuestionRequest["questions"][number],
		selectedRow: number,
		draft: DraftAnswer | undefined,
		input: Input,
		allQuestions: QuestionRequest["questions"],
	) {
		this.question = question;
		this.selectedRow = selectedRow;
		this.draft = draft;
		this.input = input;
		this.allQuestions = allQuestions;
	}

	invalidate(): void {
		this.input.invalidate();
	}

	render(width: number): string[] {
		const inputFocused = this.selectedRow === this.question.options.length;
		this.input.focused = inputFocused;
		const selectedOption = this.getDetailOption();
		if (width < DETAILS_SIDE_BY_SIDE_MIN_WIDTH) {
			const left = this.renderChoices(width);
			return [...left, "", ...this.renderDetails(width, selectedOption)];
		}

		const leftWidth = Math.max(
			DETAILS_MIN_LEFT_WIDTH,
			Math.min(Math.floor(width * 0.42), width - DETAILS_MIN_RIGHT_WIDTH - 2),
		);
		const rightWidth = Math.max(1, width - leftWidth - 2);
		const left = this.renderChoices(leftWidth);
		const right = this.renderDetails(rightWidth, selectedOption);
		const rows = Math.max(left.length, right.length);
		return Array.from({ length: rows }, (_, index) => {
			const leftLine = truncateToWidth(left[index] ?? "", leftWidth, "");
			const padding = " ".repeat(Math.max(0, leftWidth - visibleWidth(leftLine)));
			return truncateToWidth(`${leftLine}${padding}  ${right[index] ?? ""}`, width, "");
		});
	}

	private renderChoices(width: number): string[] {
		const lines: string[] = [];
		for (let index = 0; index < this.question.options.length; index++) {
			const option = this.question.options[index]!;
			const focused = this.selectedRow === index;
			const checked = this.question.multiSelect
				? this.draft?.selected?.has(option.label) === true
				: this.draft?.option === option.label;
			const marker = this.question.multiSelect ? (checked ? "[x]" : "[ ]") : checked ? "(*)" : "( )";
			const pointer = focused ? theme.fg("accent", "❯") : " ";
			const label = focused ? theme.bold(option.label) : option.label;
			const row = `${pointer} ${checked ? theme.fg("accent", marker) : marker} ${label}`;
			lines.push(...new Text(row, 1, 0, focused ? (line) => theme.bg("selectedBg", line) : undefined).render(width));
		}
		lines.push(...this.input.render(width));
		return lines;
	}

	private getDetailOption(): QuestionRequest["questions"][number]["options"][number] {
		const focused = this.question.options[this.selectedRow];
		if (focused) return focused;
		if (this.draft?.option) {
			const selected = this.question.options.find((option) => option.label === this.draft?.option);
			if (selected) return selected;
		}
		const selectedLabel = this.draft?.selected?.values().next().value;
		return this.question.options.find((option) => option.label === selectedLabel) ?? this.question.options[0]!;
	}

	private renderDetails(width: number, option: QuestionRequest["questions"][number]["options"][number]): string[] {
		const frameWidth = Math.max(12, width);
		const contentWidth = Math.max(1, frameWidth - 4);
		const title = ` ${truncateToWidth(option.label, Math.max(1, frameWidth - 6), "…")} `;
		const top = `┌${title}${"─".repeat(Math.max(0, frameWidth - visibleWidth(title) - 2))}┐`;
		const body = [
			...new Text(theme.fg("muted", option.description), 0, 0).render(contentWidth),
			...(option.preview
				? ["", ...new Markdown(option.preview, 0, 0, getMarkdownTheme()).render(contentWidth)]
				: []),
		];
		const maxBodyRows = Math.max(
			body.length,
			...this.allQuestions.flatMap((question) =>
				question.options.map((candidate) => {
					const candidateWidth = Math.max(1, frameWidth - 4);
					return (
						new Text(candidate.description, 0, 0).render(candidateWidth).length +
						(candidate.preview
							? 1 + new Markdown(candidate.preview, 0, 0, getMarkdownTheme()).render(candidateWidth).length
							: 0)
					);
				}),
			),
		);
		const paddedBody = [...body, ...Array.from({ length: maxBodyRows - body.length }, () => "")];
		return [
			theme.fg("borderAccent", fitLine(top, frameWidth)),
			...paddedBody.map((line) => `│ ${fitLine(line, contentWidth)} │`),
			theme.fg("borderAccent", `└${"─".repeat(Math.max(0, frameWidth - 2))}┘`),
		];
	}
}

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
		return Boolean(draft?.option || draft?.selected?.size || draft?.custom?.trim());
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
			this.addChild(new Text(theme.fg("warning", "Press Esc again to cancel, or any other key to return."), 1, 0));
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
		this.addChild(new Text(question.question, 1, 0));
		this.addChild(new Text(theme.fg("muted", mode), 1, 0));
		this.addChild(new Spacer(1));
		const input = this.getCustomInput(state.questionIndex);
		this.addChild(new QuestionChoicePane(question, state.selectedRow, draft, input, this.request.questions));
		if (this.error) this.addChild(new Text(theme.fg("error", this.error), 1, 0));
	}

	private renderSubmit(_state: Extract<ActiveViewState, { kind: "submit" }>): void {
		this.addChild(new Text(theme.fg("accent", theme.bold("Review your answers")), 1, 0));
		this.addChild(new Spacer(1));
		for (let index = 0; index < this.request.questions.length; index++) {
			const question = this.request.questions[index]!;
			const draft = this.getDraft(index);
			const selection = draft?.option ?? (draft?.selected?.size ? [...draft.selected].join(", ") : undefined);
			const custom = draft?.custom?.trim();
			const answer = selection ? (custom ? `${selection} · “${custom}”` : selection) : custom || "Unanswered";
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
		this.addRow("❯ Submit answers", true, this.hasAllAnswers() ? "accent" : "muted");
	}

	private renderFooter(state: ActiveViewState): void {
		const hints =
			state.kind === "submit"
				? `${rawKeyHint("←→", "switch tab")}  ${rawKeyHint("Enter", "submit")}  ${rawKeyHint("Esc", "cancel")}`
				: `${rawKeyHint("←→", "switch tab")}  ${rawKeyHint("↑↓", "navigate")}  ${rawKeyHint("Space", "select/unselect")}  ${rawKeyHint("Esc", "cancel")}`;
		this.addChild(new Text(hints, 1, 0));
	}

	private moveTab(delta: number): void {
		const total = this.request.questions.length + 1;
		const next = (this.currentTab + delta + total) % total;
		this.error = undefined;
		this.state =
			next === this.request.questions.length
				? { kind: "submit" }
				: { kind: "question", questionIndex: next, selectedRow: 0 };
		this.rebuild();
	}

	private moveRow(delta: number): void {
		const state = this.activeState;
		if (state.kind === "question") {
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
		const existing = this.getDraft(state.questionIndex) ?? {};
		if (question.multiSelect) {
			const selected = new Set(existing.selected);
			if (selected.has(option.label)) selected.delete(option.label);
			else selected.add(option.label);
			const next = { ...existing, selected: selected.size ? selected : undefined };
			if (next.selected || next.custom?.trim()) this.drafts.set(state.questionIndex, next);
			else this.drafts.delete(state.questionIndex);
		} else if (existing.option === option.label) {
			const next = { ...existing, option: undefined };
			if (next.custom?.trim()) this.drafts.set(state.questionIndex, next);
			else this.drafts.delete(state.questionIndex);
		} else {
			this.drafts.set(state.questionIndex, { ...existing, option: option.label });
		}
		this.error = undefined;
		this.rebuild();
	}

	private forwardToCustomInput(data: string): void {
		const state = this.activeState;
		if (state.kind !== "question") return;
		const input = this.getCustomInput(state.questionIndex);
		input.handleInput(data);
		const value = input.getValue().trim();
		const existing = this.getDraft(state.questionIndex) ?? {};
		const next = { ...existing, custom: value || undefined };
		if (next.option || next.selected?.size || next.custom) this.drafts.set(state.questionIndex, next);
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
			const notes = draft.custom?.trim() || undefined;
			if (question.multiSelect && draft.selected?.size) {
				return {
					questionIndex: index,
					question: question.question,
					kind: "multi",
					answer: null,
					selected: [...draft.selected],
					...(notes && { notes }),
				};
			}
			if (!question.multiSelect && draft.option) {
				return {
					questionIndex: index,
					question: question.question,
					kind: "option",
					answer: draft.option,
					...(notes && { notes }),
				};
			}
			return { questionIndex: index, question: question.question, kind: "custom", answer: notes! };
		});
		this.done({ cancelled: false, answers });
	}

	private requestCancel(): void {
		if (this.state.kind === "cancelConfirm") return;
		this.state = { kind: "cancelConfirm", returnTo: this.state };
		this.rebuild();
	}

	handleInput(data: string): void {
		const kb = getKeybindings();
		if (this.state.kind === "cancelConfirm") {
			if (matchesKey(data, Key.escape)) this.done({ cancelled: true });
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

		if (matchesKey(data, Key.escape)) {
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
				this.submitAnswers();
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
