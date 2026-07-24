import {
	BoxRenderable,
	type CliRenderer,
	type KeyEvent,
	SelectRenderable,
	SelectRenderableEvents,
	TextAttributes,
	TextareaRenderable,
	TextRenderable,
} from "@opentui/core";
import type { QuestionAnswer, QuestionRequest } from "../../../core/question.ts";
import { OPEN_TUI_COLORS } from "../opentui-design.ts";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";

type DraftAnswer =
	| { kind: "option"; value: string }
	| { kind: "custom"; value: string }
	| { kind: "multi"; values: Set<string> };

export type OpenTUIQuestionnaireResult = { cancelled: false; answers: QuestionAnswer[] } | { cancelled: true };

function consume(event: KeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

/** Persistent, non-modal structured question surface mounted above the composer. */
export class OpenTUIQuestionnaire {
	readonly root: BoxRenderable;
	private readonly request: QuestionRequest;
	private readonly done: (result: OpenTUIQuestionnaireResult) => void;
	private readonly progressNode: TextRenderable;
	private readonly questionNode: TextRenderable;
	private readonly select: SelectRenderable;
	private readonly previewNode: TextRenderable;
	private readonly customInput: TextareaRenderable;
	private readonly errorNode: TextRenderable;
	private readonly footerNode: TextRenderable;
	private readonly drafts = new Map<number, DraftAnswer>();
	private questionIndex = 0;
	private selectedRow = 0;
	private editingCustom = false;
	private completed = false;

	constructor(
		renderer: CliRenderer,
		request: QuestionRequest,
		done: (result: OpenTUIQuestionnaireResult) => void,
		initialAnswers: readonly QuestionAnswer[] = [],
	) {
		this.request = request;
		this.done = done;
		for (const answer of initialAnswers) {
			if (answer.kind === "multi") {
				this.drafts.set(answer.questionIndex, { kind: "multi", values: new Set(answer.selected ?? []) });
			} else if (answer.answer) {
				this.drafts.set(answer.questionIndex, { kind: answer.kind, value: answer.answer });
			}
		}
		this.root = new BoxRenderable(renderer, {
			width: "100%",
			flexDirection: "column",
			paddingX: 1,
			paddingY: 1,
			border: true,
			borderStyle: "rounded",
			borderColor: OPEN_TUI_COLORS.primary,
			backgroundColor: OPEN_TUI_COLORS.element,
		});
		this.progressNode = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.primary,
			attributes: TextAttributes.BOLD,
			truncate: true,
		});
		this.questionNode = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.text,
			wrapMode: "word",
		});
		this.select = new SelectRenderable(renderer, {
			width: "100%",
			height: 1,
			options: [],
			showDescription: true,
			showSelectionIndicator: true,
			wrapSelection: true,
			backgroundColor: OPEN_TUI_COLORS.element,
			textColor: OPEN_TUI_COLORS.text,
			focusedTextColor: OPEN_TUI_COLORS.text,
			selectedBackgroundColor: OPEN_TUI_COLORS.selection,
			selectedTextColor: OPEN_TUI_COLORS.selectionText,
			descriptionColor: OPEN_TUI_COLORS.muted,
			selectedDescriptionColor: OPEN_TUI_COLORS.muted,
		});
		this.select.on(SelectRenderableEvents.SELECTION_CHANGED, (index: number) => {
			this.selectedRow = index;
			this.refreshPreview();
		});
		this.select.on(SelectRenderableEvents.ITEM_SELECTED, () => this.chooseCurrent());
		this.previewNode = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.muted,
			wrapMode: "word",
			maxHeight: 4,
		});
		this.customInput = new TextareaRenderable(renderer, {
			width: "100%",
			height: 3,
			maxHeight: 6,
			placeholder: "Type a different answer",
			wrapMode: "word",
			textColor: OPEN_TUI_COLORS.text,
			focusedTextColor: OPEN_TUI_COLORS.text,
			placeholderColor: OPEN_TUI_COLORS.muted,
			cursorColor: OPEN_TUI_COLORS.primary,
			showCursor: true,
			keyBindings: [
				{ name: "return", action: "submit" },
				{ name: "return", shift: true, action: "newline" },
			],
			onSubmit: () => this.confirmCustomAnswer(),
		});
		this.customInput.visible = false;
		this.errorNode = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.error,
			wrapMode: "word",
		});
		this.errorNode.visible = false;
		this.footerNode = new TextRenderable(renderer, {
			content: "Enter choose · Tab next · Ctrl+S submit · Esc cancel",
			fg: OPEN_TUI_COLORS.dim,
			truncate: true,
		});
		this.root.add(this.progressNode);
		this.root.add(this.questionNode);
		this.root.add(this.select);
		this.root.add(this.previewNode);
		this.root.add(this.customInput);
		this.root.add(this.errorNode);
		this.root.add(this.footerNode);
		this.refresh();
	}

	get customAnswerActive(): boolean {
		return this.editingCustom;
	}

	getDraftAnswers(): QuestionAnswer[] {
		const answers: QuestionAnswer[] = [];
		for (const [questionIndex, draft] of [...this.drafts.entries()].sort(([left], [right]) => left - right)) {
			const question = this.request.questions[questionIndex];
			if (!question) continue;
			if (draft.kind === "multi") {
				answers.push({
					questionIndex,
					question: question.question,
					kind: "multi",
					answer: null,
					selected: [...draft.values],
				});
			} else {
				answers.push({ questionIndex, question: question.question, kind: draft.kind, answer: draft.value });
			}
		}
		return answers;
	}

	focus(): void {
		if (this.editingCustom) this.customInput.focus();
		else this.select.focus();
	}

	handleKey(event: KeyEvent): boolean {
		if (event.eventType === "release") return false;
		if (this.editingCustom) {
			if (matchesOpenTUIAction(event, "cancel")) {
				this.editingCustom = false;
				this.setError(undefined);
				this.refresh();
				this.select.focus();
				return consume(event);
			}
			if (matchesOpenTUIAction(event, "save")) {
				this.confirmCustomAnswer();
				return consume(event);
			}
			return false;
		}
		if (matchesOpenTUIAction(event, "cancel")) {
			this.finish({ cancelled: true });
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "up")) {
			this.select.moveUp();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "down")) {
			this.select.moveDown();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "focusLeft")) {
			this.moveQuestion(-1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "focusRight") || matchesOpenTUIAction(event, "composerAutocomplete")) {
			this.moveQuestion(1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "confirm")) {
			this.chooseCurrent();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "save")) {
			this.submit();
			return consume(event);
		}
		return false;
	}

	private get question() {
		return this.request.questions[this.questionIndex]!;
	}

	private chooseCurrent(): void {
		const question = this.question;
		if (this.selectedRow === question.options.length) {
			const draft = this.drafts.get(this.questionIndex);
			this.customInput.setText(draft?.kind === "custom" ? draft.value : "");
			this.editingCustom = true;
			this.setError(undefined);
			this.refresh();
			this.customInput.focus();
			return;
		}
		const option = question.options[this.selectedRow];
		if (!option) return;
		if (question.multiSelect) {
			const current = this.drafts.get(this.questionIndex);
			const values = current?.kind === "multi" ? new Set(current.values) : new Set<string>();
			if (values.has(option.label)) values.delete(option.label);
			else values.add(option.label);
			this.drafts.set(this.questionIndex, { kind: "multi", values });
		} else {
			this.drafts.set(this.questionIndex, { kind: "option", value: option.label });
		}
		this.setError(undefined);
		this.refreshOptions();
	}

	private confirmCustomAnswer(): void {
		const value = this.customInput.plainText.trim();
		if (!value) {
			this.setError("Custom answer must not be empty.");
			return;
		}
		this.drafts.set(this.questionIndex, { kind: "custom", value });
		this.editingCustom = false;
		this.setError(undefined);
		this.refresh();
		this.select.focus();
	}

	private moveQuestion(delta: number): void {
		const count = this.request.questions.length;
		this.questionIndex = (this.questionIndex + delta + count) % count;
		this.selectedRow = 0;
		this.editingCustom = false;
		this.setError(undefined);
		this.refresh();
		this.select.focus();
	}

	private submit(): void {
		const answers: QuestionAnswer[] = [];
		for (let index = 0; index < this.request.questions.length; index++) {
			const question = this.request.questions[index]!;
			const draft = this.drafts.get(index);
			if (!draft || (draft.kind === "multi" && draft.values.size === 0)) {
				this.questionIndex = index;
				this.selectedRow = 0;
				this.editingCustom = false;
				this.refresh();
				this.setError("Answer every question before submitting.");
				this.select.focus();
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
		this.finish({ cancelled: false, answers });
	}

	private finish(result: OpenTUIQuestionnaireResult): void {
		if (this.completed) return;
		this.completed = true;
		this.done(result);
	}

	private refresh(): void {
		const headers = this.request.questions.map((question, index) =>
			index === this.questionIndex ? `[${index + 1} ${question.header}]` : `${index + 1} ${question.header}`,
		);
		this.progressNode.content = `Agent needs your input · ${headers.join("  ")}`;
		this.questionNode.content = this.question.question;
		this.select.visible = !this.editingCustom;
		this.previewNode.visible = !this.editingCustom;
		this.customInput.visible = this.editingCustom;
		this.footerNode.content = this.editingCustom
			? "Enter apply · Shift+Enter newline · Esc back"
			: "Enter choose · Tab next · Ctrl+S submit · Esc cancel";
		this.refreshOptions();
	}

	private refreshOptions(): void {
		const question = this.question;
		const draft = this.drafts.get(this.questionIndex);
		this.select.options = [
			...question.options.map((option) => {
				const selected =
					draft?.kind === "multi"
						? draft.values.has(option.label)
						: draft?.kind === "option" && draft.value === option.label;
				const marker = question.multiSelect ? (selected ? "[x]" : "[ ]") : selected ? "(*)" : "( )";
				return { name: `${marker} ${option.label}`, description: option.description };
			}),
			{
				name: `${draft?.kind === "custom" ? "(*)" : "( )"} Custom answer`,
				description: "Type a different answer",
			},
		];
		this.select.height = Math.min(10, (question.options.length + 1) * 2);
		this.select.selectedIndex = Math.min(this.selectedRow, question.options.length);
		this.refreshPreview();
	}

	private refreshPreview(): void {
		const preview = this.question.options[this.selectedRow]?.preview;
		this.previewNode.content = preview ? `Preview: ${preview}` : "";
		this.previewNode.visible = !this.editingCustom && Boolean(preview);
	}

	private setError(message: string | undefined): void {
		this.errorNode.content = message ?? "";
		this.errorNode.visible = Boolean(message);
	}
}
