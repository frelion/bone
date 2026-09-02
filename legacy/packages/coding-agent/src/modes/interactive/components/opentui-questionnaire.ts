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

interface QuestionDraft {
	selected: Set<string>;
	notes: string;
}

interface QuestionTab {
	root: BoxRenderable;
	label: TextRenderable;
}

export interface OpenTUIQuestionnaireDraft {
	answers: QuestionAnswer[];
	overallNotes?: string;
}

export type OpenTUIQuestionnaireResult =
	| { cancelled: false; answers: QuestionAnswer[]; overallNotes?: string }
	| { cancelled: true };

type FocusArea = "options" | "questionNote" | "overallNote";

function consume(event: KeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

/** Persistent structured-question workflow with per-question and request-level notes. */
export class OpenTUIQuestionnaire {
	readonly root: BoxRenderable;
	private readonly request: QuestionRequest;
	private readonly done: (result: OpenTUIQuestionnaireResult) => void;
	private readonly progressNode: TextRenderable;
	private readonly tabs: QuestionTab[];
	private readonly questionNode: TextRenderable;
	private readonly optionHeaderNode: TextRenderable;
	private readonly select: SelectRenderable;
	private readonly detailHeaderNode: TextRenderable;
	private readonly detailTitleNode: TextRenderable;
	private readonly detailDescriptionNode: TextRenderable;
	private readonly previewNode: TextRenderable;
	private readonly questionNote: TextareaRenderable;
	private readonly reviewNode: TextRenderable;
	private readonly overallNote: TextareaRenderable;
	private readonly errorNode: TextRenderable;
	private readonly footerNode: TextRenderable;
	private readonly drafts = new Map<number, QuestionDraft>();
	private tabIndex = 0;
	private selectedRow = 0;
	private focusArea: FocusArea = "options";
	private completed = false;

	constructor(
		renderer: CliRenderer,
		request: QuestionRequest,
		done: (result: OpenTUIQuestionnaireResult) => void,
		initialDraft: OpenTUIQuestionnaireDraft = { answers: [] },
	) {
		this.request = request;
		this.done = done;
		for (const answer of initialDraft.answers) {
			const selected =
				answer.kind === "multi"
					? new Set(answer.selected ?? [])
					: answer.kind === "option" && answer.answer
						? new Set([answer.answer])
						: new Set<string>();
			this.drafts.set(answer.questionIndex, { selected, notes: answer.notes ?? "" });
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
			content: "Agent needs your input",
			fg: OPEN_TUI_COLORS.primary,
			attributes: TextAttributes.BOLD,
			truncate: true,
		});
		const tabBar = new BoxRenderable(renderer, {
			width: "100%",
			flexDirection: "row",
			gap: 1,
		});
		this.tabs = [...request.questions, undefined].map(() => {
			const tab = new BoxRenderable(renderer, {
				width: "auto",
				flexShrink: 1,
				minWidth: 7,
				paddingX: 1,
				border: ["bottom"],
				borderColor: OPEN_TUI_COLORS.border,
				backgroundColor: OPEN_TUI_COLORS.element,
			});
			const label = new TextRenderable(renderer, {
				content: "",
				fg: OPEN_TUI_COLORS.muted,
				truncate: true,
			});
			tab.add(label);
			tabBar.add(tab);
			return { root: tab, label };
		});
		this.questionNode = new TextRenderable(renderer, { content: "", fg: OPEN_TUI_COLORS.text, wrapMode: "word" });
		const contentRow = new BoxRenderable(renderer, {
			width: "100%",
			flexDirection: "row",
			gap: 1,
		});
		const optionPanel = new BoxRenderable(renderer, {
			width: "38%",
			flexShrink: 0,
			minWidth: 20,
			flexDirection: "column",
		});
		this.optionHeaderNode = new TextRenderable(renderer, {
			content: "Options",
			fg: OPEN_TUI_COLORS.muted,
			attributes: TextAttributes.BOLD,
		});
		this.select = new SelectRenderable(renderer, {
			width: "100%",
			height: 1,
			options: [],
			showDescription: false,
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
			this.refreshDetails();
		});
		this.select.on(SelectRenderableEvents.ITEM_SELECTED, () => this.toggleCurrent());
		optionPanel.add(this.optionHeaderNode);
		optionPanel.add(this.select);
		const detailPanel = new BoxRenderable(renderer, {
			flexGrow: 1,
			flexBasis: 0,
			minWidth: 0,
			flexDirection: "column",
			paddingLeft: 1,
			border: ["left"],
			borderColor: OPEN_TUI_COLORS.borderSubtle,
		});
		this.detailHeaderNode = new TextRenderable(renderer, {
			content: "Details",
			fg: OPEN_TUI_COLORS.muted,
			attributes: TextAttributes.BOLD,
		});
		this.detailTitleNode = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.primary,
			attributes: TextAttributes.BOLD,
			wrapMode: "word",
		});
		this.detailDescriptionNode = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.text,
			wrapMode: "word",
		});
		this.previewNode = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.muted,
			wrapMode: "word",
			maxHeight: 4,
		});
		this.questionNote = new TextareaRenderable(renderer, {
			width: "100%",
			height: 3,
			maxHeight: 5,
			placeholder: "Add a note for this question (optional if an option is selected)",
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
			onSubmit: () => this.finishNoteEditing(),
		});
		this.reviewNode = new TextRenderable(renderer, { content: "", fg: OPEN_TUI_COLORS.text, wrapMode: "word" });
		detailPanel.add(this.detailHeaderNode);
		detailPanel.add(this.detailTitleNode);
		detailPanel.add(this.detailDescriptionNode);
		detailPanel.add(this.previewNode);
		detailPanel.add(this.reviewNode);
		contentRow.add(optionPanel);
		contentRow.add(detailPanel);
		this.overallNote = new TextareaRenderable(renderer, {
			width: "100%",
			height: 4,
			maxHeight: 7,
			initialValue: initialDraft.overallNotes ?? "",
			placeholder: "Add notes that apply to the whole questionnaire (optional)",
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
			onSubmit: () => this.finishNoteEditing(),
		});
		this.errorNode = new TextRenderable(renderer, { content: "", fg: OPEN_TUI_COLORS.error, wrapMode: "word" });
		this.footerNode = new TextRenderable(renderer, { content: "", fg: OPEN_TUI_COLORS.dim, truncate: true });
		this.root.add(this.progressNode);
		this.root.add(tabBar);
		this.root.add(this.questionNode);
		this.root.add(contentRow);
		this.root.add(this.questionNote);
		this.root.add(this.overallNote);
		this.root.add(this.errorNode);
		this.root.add(this.footerNode);
		this.refresh();
	}

	get questionNoteActive(): boolean {
		return this.focusArea === "questionNote";
	}

	get reviewActive(): boolean {
		return this.isReview;
	}

	getDraft(): OpenTUIQuestionnaireDraft {
		return {
			answers: this.buildAnswers(false),
			...(this.overallNote.plainText.trim() && { overallNotes: this.overallNote.plainText.trim() }),
		};
	}

	focus(): void {
		if (this.focusArea === "questionNote") this.questionNote.focus();
		else if (this.focusArea === "overallNote") this.overallNote.focus();
		else this.select.focus();
	}

	handleKey(event: KeyEvent): boolean {
		if (event.eventType === "release") return false;
		if (this.focusArea !== "options") {
			if (matchesOpenTUIAction(event, "cancel")) {
				this.storeVisibleQuestionNote();
				this.focusArea = "options";
				this.select.focus();
				this.refreshFooter();
				return consume(event);
			}
			return false;
		}
		if (matchesOpenTUIAction(event, "cancel")) {
			this.finish({ cancelled: true });
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "questionPrevious")) {
			this.moveTab(-1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "questionNext")) {
			this.moveTab(1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "up")) {
			this.select.moveUp();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "down")) {
			if (!this.isReview && this.selectedRow === this.request.questions[this.tabIndex]!.options.length - 1) {
				this.focusArea = "questionNote";
				this.questionNote.focus();
				this.refreshFooter();
			} else {
				this.select.moveDown();
			}
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "confirm")) {
			this.toggleCurrent();
			return consume(event);
		}
		return false;
	}

	private get isReview(): boolean {
		return this.tabIndex === this.request.questions.length;
	}

	private draftAt(index: number): QuestionDraft {
		let draft = this.drafts.get(index);
		if (!draft) {
			draft = { selected: new Set(), notes: "" };
			this.drafts.set(index, draft);
		}
		return draft;
	}

	private toggleCurrent(): void {
		if (this.isReview) {
			if (this.selectedRow === 0) this.submit();
			else {
				this.focusArea = "overallNote";
				this.overallNote.focus();
				this.refreshFooter();
			}
			return;
		}
		const question = this.request.questions[this.tabIndex]!;
		const option = question.options[this.selectedRow];
		if (!option) return;
		const draft = this.draftAt(this.tabIndex);
		if (question.multiSelect) {
			if (draft.selected.has(option.label)) draft.selected.delete(option.label);
			else draft.selected.add(option.label);
		} else {
			if (draft.selected.has(option.label)) draft.selected.clear();
			else {
				draft.selected.clear();
				draft.selected.add(option.label);
			}
		}
		this.setError(undefined);
		this.refreshOptions();
		this.refreshTabs();
	}

	private moveTab(delta: number): void {
		const nextTabIndex = Math.max(0, Math.min(this.request.questions.length, this.tabIndex + delta));
		if (nextTabIndex === this.tabIndex) return;
		this.storeVisibleQuestionNote();
		this.tabIndex = nextTabIndex;
		this.selectedRow = 0;
		this.focusArea = "options";
		this.setError(undefined);
		this.refresh();
		this.focus();
	}

	private finishNoteEditing(): void {
		this.storeVisibleQuestionNote();
		this.focusArea = "options";
		this.select.focus();
		this.refreshTabs();
		this.refreshFooter();
	}

	private storeVisibleQuestionNote(): void {
		if (this.isReview) return;
		this.draftAt(this.tabIndex).notes = this.questionNote.plainText;
	}

	private buildAnswers(requireComplete: boolean): QuestionAnswer[] {
		const answers: QuestionAnswer[] = [];
		for (let index = 0; index < this.request.questions.length; index++) {
			const question = this.request.questions[index]!;
			const draft = this.draftAt(index);
			const notes = draft.notes.trim() || undefined;
			const selected = [...draft.selected];
			if (selected.length === 0 && !notes) {
				if (requireComplete) throw new Error(String(index));
				continue;
			}
			if (selected.length === 0) {
				answers.push({ questionIndex: index, question: question.question, kind: "note", answer: null, notes });
			} else if (question.multiSelect) {
				answers.push({
					questionIndex: index,
					question: question.question,
					kind: "multi",
					answer: null,
					selected,
					...(notes && { notes }),
				});
			} else {
				answers.push({
					questionIndex: index,
					question: question.question,
					kind: "option",
					answer: selected[0]!,
					...(notes && { notes }),
				});
			}
		}
		return answers;
	}

	private submit(): void {
		let answers: QuestionAnswer[];
		try {
			answers = this.buildAnswers(true);
		} catch (error) {
			const unansweredIndex = Number(error instanceof Error ? error.message : error);
			this.tabIndex = Number.isInteger(unansweredIndex) ? unansweredIndex : 0;
			this.focusArea = "options";
			this.selectedRow = 0;
			this.refresh();
			this.setError(`Answer question ${this.tabIndex + 1} by selecting an option or writing a note.`);
			this.select.focus();
			return;
		}
		const overallNotes = this.overallNote.plainText.trim() || undefined;
		this.finish({ cancelled: false, answers, ...(overallNotes && { overallNotes }) });
	}

	private finish(result: OpenTUIQuestionnaireResult): void {
		if (this.completed) return;
		this.completed = true;
		this.done(result);
	}

	private refresh(): void {
		this.refreshTabs();
		if (this.isReview) {
			this.questionNode.content = "Review answers and add an overall note";
			this.optionHeaderNode.content = "Actions";
			this.detailHeaderNode.content = "Summary";
			this.reviewNode.content = this.request.questions
				.map((question, index) => {
					const draft = this.draftAt(index);
					const selected = [...draft.selected].join(", ") || "No option";
					const note = draft.notes.trim() ? ` · ${draft.notes.trim().replace(/\s+/g, " ")}` : "";
					return `${index + 1}. ${question.header}: ${selected}${note}`;
				})
				.join("\n");
		} else {
			this.questionNode.content = this.request.questions[this.tabIndex]!.question;
			this.optionHeaderNode.content = "Options";
			this.detailHeaderNode.content = "Details";
			this.questionNote.setText(this.draftAt(this.tabIndex).notes);
		}
		this.select.visible = true;
		this.questionNote.visible = !this.isReview;
		this.reviewNode.visible = this.isReview;
		this.detailTitleNode.visible = !this.isReview;
		this.detailDescriptionNode.visible = !this.isReview;
		this.overallNote.visible = this.isReview;
		this.refreshOptions();
		this.refreshFooter();
	}

	private refreshTabs(): void {
		for (let index = 0; index < this.tabs.length; index++) {
			const tab = this.tabs[index]!;
			const active = index === this.tabIndex;
			const review = index === this.request.questions.length;
			const answered = !review && this.isAnswered(index);
			const header = review ? "Review" : this.request.questions[index]!.header;
			const status = active ? ">" : answered ? "✓" : " ";
			tab.label.content = `${status} ${review ? "" : `${index + 1} `}${header}`;
			tab.label.fg = active
				? OPEN_TUI_COLORS.selectionText
				: answered
					? OPEN_TUI_COLORS.text
					: OPEN_TUI_COLORS.muted;
			tab.label.attributes = active ? TextAttributes.BOLD : TextAttributes.NONE;
			tab.root.backgroundColor = active ? OPEN_TUI_COLORS.selection : OPEN_TUI_COLORS.element;
			tab.root.borderColor = active ? OPEN_TUI_COLORS.primary : OPEN_TUI_COLORS.border;
		}
	}

	private isAnswered(index: number): boolean {
		const draft = this.drafts.get(index);
		return Boolean(draft && (draft.selected.size > 0 || draft.notes.trim()));
	}

	private refreshOptions(): void {
		if (this.isReview) {
			this.select.options = [
				{ name: "Submit answers", description: "Validate and send the full questionnaire" },
				{ name: "Edit overall note", description: "Add context that applies to every answer" },
			];
			this.select.height = 2;
			this.select.selectedIndex = Math.min(this.selectedRow, 1);
			this.previewNode.visible = false;
			return;
		}
		const question = this.request.questions[this.tabIndex]!;
		const draft = this.draftAt(this.tabIndex);
		this.select.options = question.options.map((option) => ({
			name: `${question.multiSelect ? (draft.selected.has(option.label) ? "[x]" : "[ ]") : draft.selected.has(option.label) ? "(*)" : "( )"} ${option.label}`,
			description: option.description,
		}));
		this.select.height = Math.min(4, question.options.length);
		this.select.selectedIndex = Math.min(this.selectedRow, question.options.length - 1);
		this.refreshDetails();
	}

	private refreshDetails(): void {
		if (this.isReview) return;
		const option = this.request.questions[this.tabIndex]!.options[this.selectedRow];
		this.detailTitleNode.content = option?.label ?? "";
		this.detailDescriptionNode.content = option?.description ?? "";
		const preview = option?.preview;
		this.previewNode.content = preview ? `Preview: ${preview}` : "";
		this.previewNode.visible = Boolean(preview);
	}

	private refreshFooter(): void {
		this.footerNode.content = this.isReview
			? this.focusArea === "overallNote"
				? "Write overall note · Enter apply · Shift+Enter newline"
				: "Enter choose · Left/Right switch tabs · Esc cancel"
			: this.focusArea === "questionNote"
				? "Write question note · Enter apply · Shift+Enter newline"
				: "Enter select · Down after last option edits note · Left/Right switch tabs · Esc cancel";
	}

	private setError(message: string | undefined): void {
		this.errorNode.content = message ?? "";
		this.errorNode.visible = Boolean(message);
	}
}
