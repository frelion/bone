import {
	BoxRenderable,
	type CliRenderer,
	type KeyEvent,
	MarkdownRenderable,
	ScrollBoxRenderable,
	SelectRenderable,
	SelectRenderableEvents,
	SyntaxStyle,
	TextAttributes,
	TextareaRenderable,
	TextRenderable,
} from "@opentui/core";
import type { PlanProposal } from "../../../core/plan-mode.ts";
import { OPEN_TUI_COLORS } from "../opentui-design.ts";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";

export type OpenTUIPlanReviewResult =
	| { action: "approve" }
	| { action: "revise"; feedback: string }
	| { action: "cancel" };

function consume(event: KeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

/** Non-modal plan decision surface mounted above the composer. */
export class OpenTUIPlanReview {
	readonly root: BoxRenderable;
	private readonly done: (result: OpenTUIPlanReviewResult) => void;
	private readonly select: SelectRenderable;
	private readonly feedbackInput: TextareaRenderable;
	private readonly errorNode: TextRenderable;
	private readonly hintNode: TextRenderable;
	private editingFeedback = false;
	private completed = false;

	constructor(
		renderer: CliRenderer,
		proposal: PlanProposal,
		done: (result: OpenTUIPlanReviewResult) => void,
		initialFeedback = "",
	) {
		this.done = done;
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
		this.root.add(
			new TextRenderable(renderer, {
				content: `Plan v${proposal.version} is ready`,
				fg: OPEN_TUI_COLORS.primary,
				attributes: TextAttributes.BOLD,
			}),
		);
		const planBody = new ScrollBoxRenderable(renderer, {
			width: "100%",
			height: 8,
			minHeight: 4,
			border: ["top", "bottom"],
			borderColor: OPEN_TUI_COLORS.border,
			paddingX: 1,
		});
		planBody.add(
			new MarkdownRenderable(renderer, {
				content: proposal.content,
				fg: OPEN_TUI_COLORS.text,
				syntaxStyle: SyntaxStyle.fromStyles({
					default: { fg: OPEN_TUI_COLORS.text },
					"markup.heading": { fg: OPEN_TUI_COLORS.primary, bold: true },
					"markup.link": { fg: OPEN_TUI_COLORS.primary, underline: true },
					"markup.raw": { fg: OPEN_TUI_COLORS.success },
				}),
			}),
		);
		this.root.add(planBody);
		this.select = new SelectRenderable(renderer, {
			width: "100%",
			height: 6,
			options: [
				{ name: "Execute plan", description: "Approve and begin implementation", value: "approve" },
				{ name: "Revise plan", description: "Send feedback and request a replacement", value: "revise" },
				{ name: "Cancel plan", description: "Leave Plan mode without executing", value: "cancel" },
			],
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
		this.select.on(SelectRenderableEvents.ITEM_SELECTED, () => this.chooseCurrent());
		this.feedbackInput = new TextareaRenderable(renderer, {
			width: "100%",
			height: 3,
			maxHeight: 8,
			initialValue: initialFeedback,
			placeholder: "Describe what should change",
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
			onSubmit: () => this.submitFeedback(),
		});
		this.feedbackInput.visible = false;
		this.errorNode = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.error,
			wrapMode: "word",
		});
		this.errorNode.visible = false;
		this.root.add(this.select);
		this.root.add(this.feedbackInput);
		this.root.add(this.errorNode);
		this.hintNode = new TextRenderable(renderer, {
			content: "Enter choose · Up/Down move · scroll plan with mouse",
			fg: OPEN_TUI_COLORS.dim,
			truncate: true,
		});
		this.root.add(this.hintNode);
	}

	get feedbackActive(): boolean {
		return this.editingFeedback;
	}

	getDraftFeedback(): string {
		return this.feedbackInput.plainText;
	}

	focus(): void {
		if (this.editingFeedback) this.feedbackInput.focus();
		else this.select.focus();
	}

	handleKey(event: KeyEvent): boolean {
		if (event.eventType === "release") return false;
		if (this.editingFeedback) {
			if (matchesOpenTUIAction(event, "cancel")) {
				this.editingFeedback = false;
				this.setError(undefined);
				this.refresh();
				this.select.focus();
				return consume(event);
			}
			if (matchesOpenTUIAction(event, "save")) {
				this.submitFeedback();
				return consume(event);
			}
			return false;
		}
		if (matchesOpenTUIAction(event, "up")) {
			this.select.moveUp();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "down")) {
			this.select.moveDown();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "confirm")) {
			this.chooseCurrent();
			return consume(event);
		}
		return false;
	}

	private chooseCurrent(): void {
		const action = this.select.getSelectedOption()?.value;
		if (action === "approve") this.finish({ action });
		else if (action === "cancel") this.finish({ action });
		else if (action === "revise") {
			this.editingFeedback = true;
			this.setError(undefined);
			this.refresh();
			this.feedbackInput.focus();
		}
	}

	private submitFeedback(): void {
		const feedback = this.feedbackInput.plainText.trim();
		if (!feedback) {
			this.setError("Revision feedback must not be empty.");
			return;
		}
		this.finish({ action: "revise", feedback });
	}

	private finish(result: OpenTUIPlanReviewResult): void {
		if (this.completed) return;
		this.completed = true;
		this.done(result);
	}

	private refresh(): void {
		this.select.visible = !this.editingFeedback;
		this.feedbackInput.visible = this.editingFeedback;
		this.hintNode.content = this.editingFeedback
			? "Enter submit · Shift+Enter newline · Esc back"
			: "Enter choose · Up/Down move · scroll plan with mouse";
	}

	private setError(message: string | undefined): void {
		this.errorNode.content = message ?? "";
		this.errorNode.visible = Boolean(message);
	}
}
