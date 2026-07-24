import type { AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions } from "@frelion/bone-tui";
import {
	BoxRenderable,
	type CliRenderer,
	type KeyEvent,
	SelectRenderable,
	TextareaRenderable,
	TextRenderable,
} from "@opentui/core";
import { OPEN_TUI_COLORS } from "../opentui-design.ts";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";
import type { Theme } from "../theme/theme.ts";

const MAX_HISTORY_SIZE = 100;
const DEFAULT_AUTOCOMPLETE_ROWS = 5;

export interface OpenTUIComposerOptions {
	placeholder?: string;
	status?: Partial<OpenTUIComposerStatus>;
	interactionState?: OpenTUIComposerInteractionState;
	queuedMessages?: readonly OpenTUIQueuedMessage[];
	history?: readonly string[];
	autocompleteProvider?: AutocompleteProvider;
	autocompleteMaxVisible?: number;
	theme?: Theme;
	onChange?: (value: string) => void;
	onSubmit?: (value: string) => void;
	onCancel?: () => void;
}

export interface OpenTUIComposerStatus {
	cwd: string;
	model: string;
	thinking: string;
	contextRemaining: string;
	foregroundThroughput: string;
}

export type OpenTUIComposerInteractionKind = "idle" | "working" | "waiting";

export interface OpenTUIComposerInteractionState {
	kind: OpenTUIComposerInteractionKind;
	placeholder?: string;
	leftHint?: string;
	rightHint?: string;
}

export interface OpenTUIQueuedMessage {
	id: string;
	text: string;
}

const DEFAULT_STATUS: OpenTUIComposerStatus = {
	cwd: ".",
	model: "No model",
	thinking: "off",
	contextRemaining: "--",
	foregroundThroughput: "idle",
};

const DEFAULT_INTERACTION_COPY: Record<
	OpenTUIComposerInteractionKind,
	{ placeholder: string; leftHint?: string; rightHint?: string }
> = {
	idle: { placeholder: "Ask anything" },
	working: {
		placeholder: "Add instructions to the current task",
		leftHint: "Enter add to current task · Alt+Enter queue",
		rightHint: "Ctrl+C stop",
	},
	waiting: {
		placeholder: "Type your answer",
		leftHint: "Enter answer",
		rightHint: "Esc cancel",
	},
};

function copyQueuedMessages(messages: readonly OpenTUIQueuedMessage[]): OpenTUIQueuedMessage[] {
	return messages.map((message) => ({ id: message.id, text: message.text }));
}

function consume(event: KeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

function cursorPosition(value: string, offset: number): { lines: string[]; cursorLine: number; cursorCol: number } {
	const lines = value.split("\n");
	let remaining = Math.max(0, Math.min(offset, value.length));
	for (let cursorLine = 0; cursorLine < lines.length; cursorLine++) {
		const line = lines[cursorLine] ?? "";
		if (remaining <= line.length) return { lines, cursorLine, cursorCol: remaining };
		remaining -= line.length + 1;
	}
	const cursorLine = Math.max(0, lines.length - 1);
	return { lines, cursorLine, cursorCol: lines[cursorLine]?.length ?? 0 };
}

function cursorOffset(lines: string[], cursorLine: number, cursorCol: number): number {
	const lineIndex = Math.max(0, Math.min(cursorLine, Math.max(0, lines.length - 1)));
	let offset = 0;
	for (let index = 0; index < lineIndex; index++) offset += (lines[index]?.length ?? 0) + 1;
	return offset + Math.max(0, Math.min(cursorCol, lines[lineIndex]?.length ?? 0));
}

/** Structured OpenTUI prompt composer with fixed Bone product key actions. */
export class OpenTUIComposer {
	readonly root: BoxRenderable;
	readonly input: TextareaRenderable;
	readonly autocomplete: SelectRenderable;
	public onChange: ((value: string) => void) | undefined;
	public onSubmit: ((value: string) => void) | undefined;
	public onCancel: (() => void) | undefined;
	public onFocusRequest: (() => void) | undefined;
	private placeholderValue: string;
	private status: OpenTUIComposerStatus;
	private interactionState: OpenTUIComposerInteractionState;
	private queuedMessages: OpenTUIQueuedMessage[];
	private autocompleteProvider: AutocompleteProvider | undefined;
	private readonly autocompleteMaxVisible: number;
	private history: string[];
	private historyIndex = -1;
	private historyDraft = "";
	private readonly promptRoot: BoxRenderable;
	private readonly queuePanel: BoxRenderable;
	private readonly queueHeader: TextRenderable;
	private readonly queueItems: TextRenderable;
	private readonly statusLeft: TextRenderable;
	private readonly statusRight: TextRenderable;
	private autocompleteSuggestions: AutocompleteSuggestions | undefined;
	private autocompleteAbort: AbortController | undefined;
	private autocompleteGeneration = 0;
	private suppressedAutocompleteValue: string | undefined;
	private internalChange = false;
	private currentValue = "";
	private focused = false;
	private destroyed = false;

	constructor(renderer: CliRenderer, options: OpenTUIComposerOptions = {}) {
		this.placeholderValue = options.placeholder ?? "Ask anything";
		this.status = { ...DEFAULT_STATUS, ...options.status };
		this.interactionState = { kind: "idle", ...options.interactionState };
		this.queuedMessages = copyQueuedMessages(options.queuedMessages ?? []);
		this.autocompleteProvider = options.autocompleteProvider;
		const autocompleteRows = options.autocompleteMaxVisible ?? DEFAULT_AUTOCOMPLETE_ROWS;
		this.autocompleteMaxVisible = Number.isFinite(autocompleteRows)
			? Math.max(1, Math.floor(autocompleteRows))
			: DEFAULT_AUTOCOMPLETE_ROWS;
		this.history = [];
		this.setHistory(options.history ?? []);
		this.onChange = options.onChange;
		this.onSubmit = options.onSubmit;
		this.onCancel = options.onCancel;

		this.root = new BoxRenderable(renderer, {
			width: "100%",
			flexDirection: "column",
			onMouseDown: () => this.onFocusRequest?.(),
		});
		this.autocomplete = new SelectRenderable(renderer, {
			width: "100%",
			height: 1,
			options: [],
			showDescription: true,
			showSelectionIndicator: true,
			wrapSelection: true,
			...this.selectStyle(),
		});
		this.autocomplete.visible = false;
		this.queuePanel = new BoxRenderable(renderer, {
			width: "100%",
			flexDirection: "column",
			paddingX: 1,
		});
		this.queueHeader = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.muted,
			height: 1,
		});
		this.queueItems = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.dim,
			truncate: true,
			height: 1,
		});
		this.queuePanel.add(this.queueHeader);
		this.queuePanel.add(this.queueItems);
		this.promptRoot = new BoxRenderable(renderer, {
			width: "100%",
			flexDirection: "column",
			paddingX: 1,
			border: true,
			borderStyle: "rounded",
			borderColor: OPEN_TUI_COLORS.border,
			backgroundColor: OPEN_TUI_COLORS.page,
			onMouseDown: (event) => {
				event.stopPropagation();
				this.onFocusRequest?.();
			},
		});
		this.input = new TextareaRenderable(renderer, {
			width: "100%",
			height: 1,
			minWidth: 0,
			minHeight: 1,
			maxHeight: 8,
			placeholder: this.placeholderValue,
			wrapMode: "word",
			keyBindings: [
				{ name: "return", action: "submit" },
				{ name: "return", shift: true, action: "newline" },
				{ name: "j", ctrl: true, action: "newline" },
			],
			...this.textareaStyle(),
			onContentChange: () => this.handleTextChange(this.input.plainText),
			onSubmit: () => this.submit(),
			onKeyDown: (event) => {
				if (!matchesOpenTUIAction(event, "composerCancel")) return;
				event.preventDefault();
				event.stopPropagation();
				this.cancel();
			},
		});
		const statusRow = new BoxRenderable(renderer, {
			width: "100%",
			height: 1,
			flexDirection: "row",
			alignItems: "center",
			gap: 1,
		});
		this.statusLeft = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.muted,
			truncate: true,
			flexGrow: 1,
			minWidth: 0,
			height: 1,
		});
		this.statusRight = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.dim,
			truncate: true,
			flexShrink: 1,
			minWidth: 0,
			height: 1,
		});
		statusRow.add(this.statusLeft);
		statusRow.add(this.statusRight);
		this.root.add(this.autocomplete);
		this.root.add(this.queuePanel);
		this.promptRoot.add(this.input);
		this.promptRoot.add(statusRow);
		this.root.add(this.promptRoot);
		this.input.on("focused", () => {
			this.focused = true;
			this.refreshFocusStyle();
		});
		this.input.on("blurred", () => {
			this.focused = false;
			this.refreshFocusStyle();
		});
		this.refreshStatus();
		this.refreshInteractionPresentation();
		this.refreshQueuePanel();
		this.refreshFocusStyle();
	}

	get value(): string {
		return this.currentValue;
	}

	/** Return the actual edit node used by application-level pane focus. */
	get focusNode(): TextareaRenderable {
		return this.input;
	}

	get autocompleteOpen(): boolean {
		return Boolean(this.autocompleteSuggestions);
	}

	get selectedAutocompleteItem(): AutocompleteItem | undefined {
		if (this.nativeDestroyed) return undefined;
		return this.autocomplete.getSelectedOption()?.value as AutocompleteItem | undefined;
	}

	get interactionKind(): OpenTUIComposerInteractionKind {
		return this.interactionState.kind;
	}

	get queuedMessageCount(): number {
		return this.queuedMessages.length;
	}

	handleKey(event: KeyEvent): boolean {
		if (this.nativeDestroyed) return false;
		if (event.eventType === "release") return false;
		if (this.autocompleteOpen) {
			if (matchesOpenTUIAction(event, "composerCancel")) {
				this.dismissAutocomplete();
				return consume(event);
			}
			if (matchesOpenTUIAction(event, "composerHistoryUp")) {
				this.autocomplete.moveUp();
				return consume(event);
			}
			if (matchesOpenTUIAction(event, "composerHistoryDown")) {
				this.autocomplete.moveDown();
				return consume(event);
			}
			if (matchesOpenTUIAction(event, "composerSubmit") || matchesOpenTUIAction(event, "composerAutocomplete")) {
				const item = this.autocomplete.getSelectedOption()?.value as AutocompleteItem | undefined;
				if (item) {
					const submitAfterCompletion =
						matchesOpenTUIAction(event, "composerSubmit") && this.hasUniqueSlashCommandSuggestion();
					this.applyAutocomplete(item);
					if (submitAfterCompletion) this.submit();
				}
				return consume(event);
			}
		}
		if (matchesOpenTUIAction(event, "composerAutocomplete")) {
			void this.requestAutocomplete(true);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "composerNewline") || matchesOpenTUIAction(event, "composerNewlineAlt")) {
			this.insertNewline();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "composerSubmit")) {
			this.submit();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "composerHistoryUp")) {
			this.navigateHistory(1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "composerHistoryDown")) {
			this.navigateHistory(-1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "composerCancel")) {
			this.cancel();
			return consume(event);
		}
		return false;
	}

	focus(): void {
		this.focused = true;
		if (this.nativeDestroyed) return;
		this.input.focus();
		this.refreshFocusStyle();
	}

	blur(): void {
		this.focused = false;
		if (this.nativeDestroyed) return;
		this.input.blur();
		this.refreshFocusStyle();
	}

	setValue(value: string, cursor = value.length): void {
		this.setTextareaValue(value, cursor, true);
		this.exitHistory();
		void this.requestAutocomplete(false);
	}

	setPlaceholder(placeholder: string): void {
		this.placeholderValue = placeholder;
		this.refreshInteractionPresentation();
	}

	setInteractionState(state: OpenTUIComposerInteractionState): void {
		this.interactionState = { ...state };
		this.refreshInteractionPresentation();
	}

	setQueuedMessages(messages: readonly OpenTUIQueuedMessage[]): void {
		this.queuedMessages = copyQueuedMessages(messages);
		this.refreshQueuePanel();
	}

	getQueuedMessages(): readonly OpenTUIQueuedMessage[] {
		return copyQueuedMessages(this.queuedMessages);
	}

	dismissAutocomplete(): boolean {
		if (!this.autocompleteOpen) return false;
		this.closeAutocomplete();
		return true;
	}

	updateStatus(status: Partial<OpenTUIComposerStatus>): void {
		this.status = { ...this.status, ...status };
		this.refreshStatus();
	}

	setHistory(history: readonly string[]): void {
		this.history = history
			.map((entry) => entry.trim())
			.filter((entry) => entry.length > 0)
			.slice(0, MAX_HISTORY_SIZE);
		this.exitHistory();
	}

	addHistoryEntry(value: string): void {
		const entry = value.trim();
		if (!entry || this.history[0] === entry) return;
		this.history.unshift(entry);
		if (this.history.length > MAX_HISTORY_SIZE) this.history.length = MAX_HISTORY_SIZE;
	}

	setAutocompleteProvider(provider: AutocompleteProvider | undefined): void {
		this.autocompleteProvider = provider;
		this.closeAutocomplete();
	}

	updateTheme(_nextTheme: Theme): void {
		if (this.nativeDestroyed) return;
		this.promptRoot.borderColor = this.focused ? OPEN_TUI_COLORS.primary : OPEN_TUI_COLORS.border;
		const textareaStyle = this.textareaStyle();
		this.input.textColor = textareaStyle.textColor;
		this.input.focusedTextColor = textareaStyle.focusedTextColor;
		this.input.placeholderColor = textareaStyle.placeholderColor;
		this.input.cursorColor = textareaStyle.cursorColor;
		const selectStyle = this.selectStyle();
		this.autocomplete.textColor = selectStyle.textColor;
		this.autocomplete.focusedTextColor = selectStyle.focusedTextColor;
		this.autocomplete.selectedBackgroundColor = selectStyle.selectedBackgroundColor;
		this.autocomplete.selectedTextColor = selectStyle.selectedTextColor;
		this.autocomplete.descriptionColor = selectStyle.descriptionColor;
		this.autocomplete.selectedDescriptionColor = selectStyle.selectedDescriptionColor;
		this.statusLeft.fg = OPEN_TUI_COLORS.muted;
		this.statusRight.fg = OPEN_TUI_COLORS.dim;
		this.queueHeader.fg = OPEN_TUI_COLORS.muted;
		this.queueItems.fg = OPEN_TUI_COLORS.dim;
		this.refreshFocusStyle();
	}

	destroy(): void {
		if (this.destroyed) return;
		this.destroyed = true;
		this.invalidateAutocomplete();
		if (!this.root.isDestroyed) this.root.destroyRecursively();
	}

	private get nativeDestroyed(): boolean {
		return this.destroyed || this.root.isDestroyed;
	}

	private textareaStyle() {
		return {
			textColor: OPEN_TUI_COLORS.text,
			focusedTextColor: OPEN_TUI_COLORS.text,
			placeholderColor: OPEN_TUI_COLORS.muted,
			cursorColor: OPEN_TUI_COLORS.primary,
			showCursor: true,
		} as const;
	}

	private selectStyle() {
		return {
			textColor: OPEN_TUI_COLORS.text,
			focusedTextColor: OPEN_TUI_COLORS.text,
			selectedBackgroundColor: OPEN_TUI_COLORS.selection,
			selectedTextColor: OPEN_TUI_COLORS.selectionText,
			descriptionColor: OPEN_TUI_COLORS.muted,
			selectedDescriptionColor: OPEN_TUI_COLORS.muted,
		} as const;
	}

	private handleTextChange(value: string): void {
		if (this.internalChange) return;
		const suppressAutocomplete = this.suppressedAutocompleteValue === value;
		this.suppressedAutocompleteValue = undefined;
		this.currentValue = value;
		this.refreshTextareaHeight(value);
		this.exitHistory();
		this.onChange?.(value);
		if (!suppressAutocomplete) void this.requestAutocomplete(false);
	}

	private setTextareaValue(value: string, cursor: number, notify: boolean): void {
		const changed = this.currentValue !== value;
		this.currentValue = value;
		const textarea = this.input;
		if (textarea.isDestroyed) {
			if (changed && notify) this.onChange?.(value);
			return;
		}
		this.internalChange = true;
		textarea.setText(value);
		textarea.cursorOffset = Math.max(0, Math.min(cursor, value.length));
		this.refreshTextareaHeight(value);
		this.internalChange = false;
		if (changed && notify) this.onChange?.(value);
	}

	private refreshTextareaHeight(value: string): void {
		if (this.input.isDestroyed) return;
		this.input.height = Math.min(8, Math.max(1, value.split("\n").length));
	}

	private refreshFocusStyle(): void {
		if (this.promptRoot.isDestroyed) return;
		this.promptRoot.borderColor = this.focused ? OPEN_TUI_COLORS.primary : OPEN_TUI_COLORS.border;
	}

	private refreshStatus(): void {
		if (this.statusLeft.isDestroyed || this.statusRight.isDestroyed) return;
		const defaults = DEFAULT_INTERACTION_COPY[this.interactionState.kind];
		this.statusLeft.content =
			this.interactionState.leftHint ??
			defaults.leftHint ??
			`${this.status.cwd}  ${this.status.model}  ${this.status.thinking}`;
		this.statusRight.content =
			this.interactionState.rightHint ??
			defaults.rightHint ??
			`${this.status.contextRemaining} left  ${this.status.foregroundThroughput}`;
	}

	private refreshInteractionPresentation(): void {
		if (this.input.isDestroyed) return;
		const defaults = DEFAULT_INTERACTION_COPY[this.interactionState.kind];
		this.input.placeholder =
			this.interactionState.placeholder ??
			(this.interactionState.kind === "idle" ? this.placeholderValue : defaults.placeholder);
		this.refreshStatus();
	}

	private refreshQueuePanel(): void {
		if (this.queuePanel.isDestroyed || this.queueHeader.isDestroyed || this.queueItems.isDestroyed) return;
		const count = this.queuedMessages.length;
		this.queuePanel.visible = count > 0;
		if (count === 0) {
			this.queueHeader.content = "";
			this.queueItems.content = "";
			this.queueItems.height = 1;
			return;
		}
		this.queueHeader.content = `Queued next: ${count}`;
		const visibleMessages = this.queuedMessages.slice(0, 3);
		const lines = visibleMessages.map((message, index) => {
			const summary = message.text.replace(/\s+/g, " ").trim();
			return `  ${index + 1}. ${summary}`;
		});
		if (count > visibleMessages.length) lines.push(`  +${count - visibleMessages.length} more`);
		this.queueItems.content = lines.join("\n");
		this.queueItems.height = lines.length;
	}

	private insertNewline(): void {
		const textarea = this.input;
		this.exitHistory();
		textarea.insertText("\n");
	}

	private submit(): void {
		const value = this.value.trim();
		this.closeAutocomplete();
		if (!value) return;
		this.addHistoryEntry(value);
		this.setTextareaValue("", 0, true);
		this.exitHistory();
		this.onSubmit?.(value);
	}

	private cancel(): void {
		if (this.dismissAutocomplete()) return;
		this.onCancel?.();
	}

	private navigateHistory(direction: 1 | -1): void {
		if (this.history.length === 0) return;
		if (direction === 1) {
			if (this.historyIndex === -1) this.historyDraft = this.value;
			if (this.historyIndex >= this.history.length - 1) return;
			this.historyIndex++;
			const value = this.history[this.historyIndex] ?? "";
			this.setTextareaValue(value, value.length, true);
			this.closeAutocomplete();
			return;
		}
		if (this.historyIndex === -1) return;
		this.historyIndex--;
		const value = this.historyIndex === -1 ? this.historyDraft : (this.history[this.historyIndex] ?? "");
		this.setTextareaValue(value, value.length, true);
		this.closeAutocomplete();
	}

	private exitHistory(): void {
		this.historyIndex = -1;
		this.historyDraft = "";
	}

	private async requestAutocomplete(force: boolean): Promise<void> {
		const provider = this.autocompleteProvider;
		const textarea = this.input;
		if (!provider || textarea.isDestroyed || this.nativeDestroyed) {
			this.closeAutocomplete();
			return;
		}
		const position = cursorPosition(textarea.plainText, textarea.cursorOffset);
		const generation = ++this.autocompleteGeneration;
		this.autocompleteAbort?.abort();
		const controller = new AbortController();
		this.autocompleteAbort = controller;
		try {
			const suggestions = await provider.getSuggestions(position.lines, position.cursorLine, position.cursorCol, {
				signal: controller.signal,
				force,
			});
			if (controller.signal.aborted || generation !== this.autocompleteGeneration || this.nativeDestroyed) return;
			this.autocompleteAbort = undefined;
			if (!suggestions || suggestions.items.length === 0) {
				this.closeAutocomplete();
				return;
			}
			this.showAutocomplete(suggestions);
		} catch {
			if (!controller.signal.aborted && generation === this.autocompleteGeneration) this.closeAutocomplete();
		}
	}

	private showAutocomplete(suggestions: AutocompleteSuggestions): void {
		if (this.nativeDestroyed) return;
		const autocomplete = this.autocomplete;
		this.autocompleteSuggestions = suggestions;
		autocomplete.options = suggestions.items.map((item) => ({
			name: item.label,
			description: item.description ?? "",
			value: item,
		}));
		autocomplete.selectedIndex = 0;
		autocomplete.height = Math.min(this.autocompleteMaxVisible, suggestions.items.length);
		autocomplete.visible = true;
	}

	private applyAutocomplete(item: AutocompleteItem): void {
		const provider = this.autocompleteProvider;
		const suggestions = this.autocompleteSuggestions;
		const textarea = this.input;
		if (!provider || !suggestions || textarea.isDestroyed || this.nativeDestroyed) return;
		const position = cursorPosition(textarea.plainText, textarea.cursorOffset);
		const result = provider.applyCompletion(
			position.lines,
			position.cursorLine,
			position.cursorCol,
			item,
			suggestions.prefix,
		);
		const value = result.lines.join("\n");
		this.exitHistory();
		this.suppressedAutocompleteValue = value;
		this.setTextareaValue(value, cursorOffset(result.lines, result.cursorLine, result.cursorCol), true);
		this.closeAutocomplete();
	}

	private hasUniqueSlashCommandSuggestion(): boolean {
		const suggestions = this.autocompleteSuggestions;
		if (!suggestions || suggestions.items.length !== 1) return false;
		return /^\/[^/\s]*$/.test(suggestions.prefix);
	}

	private closeAutocomplete(): void {
		this.invalidateAutocomplete();
		this.autocompleteSuggestions = undefined;
		if (this.autocomplete.isDestroyed) return;
		this.autocomplete.visible = false;
		this.autocomplete.options = [];
		this.autocomplete.height = 1;
	}

	private invalidateAutocomplete(): void {
		this.autocompleteGeneration++;
		this.autocompleteAbort?.abort();
		this.autocompleteAbort = undefined;
	}
}
