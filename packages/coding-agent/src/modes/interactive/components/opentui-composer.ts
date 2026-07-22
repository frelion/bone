import type {
	AutocompleteItem,
	AutocompleteProvider,
	AutocompleteSuggestions,
	BoneContainerNode,
	BoneKeyEvent,
	BoneNode,
	BoneRenderContext,
	BoneSelectNode,
	BoneTextareaNode,
	BoneTextNode,
	BoneView,
} from "@frelion/bone-tui";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";
import { type Theme, theme } from "../theme/theme.ts";

const MAX_HISTORY_SIZE = 100;
const DEFAULT_AUTOCOMPLETE_ROWS = 5;

export interface OpenTUIComposerOptions {
	placeholder?: string;
	history?: readonly string[];
	autocompleteProvider?: AutocompleteProvider;
	autocompleteMaxVisible?: number;
	theme?: Theme;
	onChange?: (value: string) => void;
	onSubmit?: (value: string) => void;
	onCancel?: () => void;
}

function consume(event: BoneKeyEvent): true {
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
export class OpenTUIComposer implements BoneView {
	public onChange: ((value: string) => void) | undefined;
	public onSubmit: ((value: string) => void) | undefined;
	public onCancel: (() => void) | undefined;
	private composerTheme: Theme;
	private placeholderValue: string;
	private autocompleteProvider: AutocompleteProvider | undefined;
	private readonly autocompleteMaxVisible: number;
	private history: string[];
	private historyIndex = -1;
	private historyDraft = "";
	private root: BoneContainerNode | undefined;
	private promptRoot: BoneContainerNode | undefined;
	private focusMarker: BoneTextNode | undefined;
	private textarea: BoneTextareaNode | undefined;
	private autocomplete: BoneSelectNode<AutocompleteItem> | undefined;
	private autocompleteSuggestions: AutocompleteSuggestions | undefined;
	private autocompleteAbort: AbortController | undefined;
	private autocompleteGeneration = 0;
	private internalChange = false;
	private currentValue = "";
	private focused = false;
	private destroyed = false;

	constructor(options: OpenTUIComposerOptions = {}) {
		this.composerTheme = options.theme ?? theme;
		this.placeholderValue = options.placeholder ?? "Message Bone";
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
	}

	get value(): string {
		return this.currentValue;
	}

	get autocompleteOpen(): boolean {
		return Boolean(this.autocompleteSuggestions);
	}

	get selectedAutocompleteItem(): AutocompleteItem | undefined {
		if (this.nativeDestroyed) return undefined;
		return this.autocomplete?.selectedItem?.value;
	}

	mount(context: BoneRenderContext): BoneNode {
		if (this.root) throw new Error("OpenTUIComposer is already mounted");
		const root = context.createBox({
			width: "100%",
			flexDirection: "column",
			onMouseDown: () => this.textarea?.focus(),
		});
		const autocomplete = context.createSelect<AutocompleteItem>({
			width: "100%",
			height: 1,
			items: [],
			showDescription: true,
			showSelectionIndicator: true,
			wrapSelection: true,
			...this.selectStyle(),
			onConfirm: (item) => this.applyAutocomplete(item.value),
			onCancel: () => this.closeAutocomplete(),
		});
		autocomplete.visible = false;
		const promptRoot = context.createBox({
			width: "100%",
			flexDirection: "row",
			paddingX: 1,
			gap: 1,
			backgroundColor: this.composerTheme.getBgColor("customMessageBg"),
			onMouseDown: () => this.focus(),
		});
		const focusMarker = context.createText({
			content: "›",
			fg: this.composerTheme.getFgColor("muted"),
			flexShrink: 0,
		});
		const textarea = context.createTextarea({
			flexGrow: 1,
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
			onChange: (value) => this.handleTextChange(value),
			onSubmit: () => this.submit(),
			onCancel: () => this.cancel(),
		});
		root.append(autocomplete);
		promptRoot.append(focusMarker);
		promptRoot.append(textarea);
		root.append(promptRoot);
		this.root = root;
		this.promptRoot = promptRoot;
		this.focusMarker = focusMarker;
		this.autocomplete = autocomplete;
		this.textarea = textarea;
		this.refreshFocusStyle();
		return root;
	}

	handleKey(event: BoneKeyEvent): boolean {
		if (this.nativeDestroyed) return false;
		if (event.eventType === "release") return false;
		if (this.autocompleteOpen) {
			if (matchesOpenTUIAction(event, "composerCancel")) {
				this.closeAutocomplete();
				return consume(event);
			}
			if (matchesOpenTUIAction(event, "composerHistoryUp")) {
				this.autocomplete?.moveUp();
				return consume(event);
			}
			if (matchesOpenTUIAction(event, "composerHistoryDown")) {
				this.autocomplete?.moveDown();
				return consume(event);
			}
			if (matchesOpenTUIAction(event, "composerSubmit") || matchesOpenTUIAction(event, "composerAutocomplete")) {
				const item = this.autocomplete?.selectedItem;
				if (item) this.applyAutocomplete(item.value);
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
		this.textarea?.focus();
		this.refreshFocusStyle();
	}

	blur(): void {
		this.focused = false;
		if (this.nativeDestroyed) return;
		this.textarea?.blur();
		this.refreshFocusStyle();
	}

	setValue(value: string, cursor = value.length): void {
		this.setTextareaValue(value, cursor, true);
		this.exitHistory();
		void this.requestAutocomplete(false);
	}

	setPlaceholder(placeholder: string): void {
		this.placeholderValue = placeholder;
		if (this.textarea && !this.textarea.destroyed) this.textarea.placeholder = placeholder;
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

	updateTheme(nextTheme: Theme): void {
		this.composerTheme = nextTheme;
		if (this.nativeDestroyed) return;
		this.promptRoot?.updateStyle({ backgroundColor: nextTheme.getBgColor("customMessageBg") });
		this.textarea?.updateStyle(this.textareaStyle());
		this.autocomplete?.updateStyle(this.selectStyle());
		this.refreshFocusStyle();
	}

	destroy(): void {
		if (this.destroyed) return;
		this.destroyed = true;
		this.invalidateAutocomplete();
		if (this.root && !this.root.destroyed) this.root.destroy();
	}

	private get nativeDestroyed(): boolean {
		return this.destroyed || this.root?.destroyed === true;
	}

	private textareaStyle() {
		return {
			textColor: this.composerTheme.getFgColor("text"),
			focusedTextColor: this.composerTheme.getFgColor("text"),
			placeholderColor: this.composerTheme.getFgColor("muted"),
			cursorColor: this.composerTheme.getFgColor("accent"),
			showCursor: true,
		} as const;
	}

	private selectStyle() {
		return {
			textColor: this.composerTheme.getFgColor("text"),
			focusedTextColor: this.composerTheme.getFgColor("text"),
			selectedBackgroundColor: this.composerTheme.getBgColor("selectedBg"),
			selectedTextColor: this.composerTheme.getFgColor("text"),
			descriptionColor: this.composerTheme.getFgColor("muted"),
			selectedDescriptionColor: this.composerTheme.getFgColor("muted"),
		} as const;
	}

	private handleTextChange(value: string): void {
		if (this.internalChange) return;
		this.currentValue = value;
		this.exitHistory();
		this.onChange?.(value);
		void this.requestAutocomplete(false);
	}

	private setTextareaValue(value: string, cursor: number, notify: boolean): void {
		const changed = this.currentValue !== value;
		this.currentValue = value;
		const textarea = this.textarea;
		if (!textarea || textarea.destroyed) {
			if (changed && notify) this.onChange?.(value);
			return;
		}
		this.internalChange = true;
		textarea.value = value;
		textarea.setCursorOffset(Math.max(0, Math.min(cursor, value.length)));
		this.internalChange = false;
		if (changed && notify) this.onChange?.(value);
	}

	private refreshFocusStyle(): void {
		if (!this.focusMarker || this.focusMarker.destroyed) return;
		this.focusMarker.updateStyle({
			fg: this.composerTheme.getFgColor(this.focused ? "accent" : "muted"),
			bold: this.focused,
		});
	}

	private insertNewline(): void {
		const textarea = this.requireTextarea();
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
		if (this.autocompleteOpen) {
			this.closeAutocomplete();
			return;
		}
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
		const textarea = this.textarea;
		if (!provider || !textarea || textarea.destroyed || this.nativeDestroyed) {
			this.closeAutocomplete();
			return;
		}
		const position = cursorPosition(textarea.value, textarea.cursorOffset);
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
		const autocomplete = this.requireAutocomplete();
		this.autocompleteSuggestions = suggestions;
		autocomplete.items = suggestions.items.map((item) => ({
			label: item.label,
			description: item.description,
			value: item,
		}));
		autocomplete.selectedIndex = 0;
		autocomplete.updateLayout({ height: Math.min(this.autocompleteMaxVisible, suggestions.items.length) });
		autocomplete.visible = true;
	}

	private applyAutocomplete(item: AutocompleteItem): void {
		const provider = this.autocompleteProvider;
		const suggestions = this.autocompleteSuggestions;
		const textarea = this.textarea;
		if (!provider || !suggestions || !textarea || textarea.destroyed || this.nativeDestroyed) return;
		const position = cursorPosition(textarea.value, textarea.cursorOffset);
		const result = provider.applyCompletion(
			position.lines,
			position.cursorLine,
			position.cursorCol,
			item,
			suggestions.prefix,
		);
		const value = result.lines.join("\n");
		this.closeAutocomplete();
		this.exitHistory();
		this.setTextareaValue(value, cursorOffset(result.lines, result.cursorLine, result.cursorCol), true);
	}

	private closeAutocomplete(): void {
		this.invalidateAutocomplete();
		this.autocompleteSuggestions = undefined;
		if (this.autocomplete && !this.autocomplete.destroyed) {
			this.autocomplete.visible = false;
			this.autocomplete.items = [];
			this.autocomplete.updateLayout({ height: 1 });
		}
	}

	private invalidateAutocomplete(): void {
		this.autocompleteGeneration++;
		this.autocompleteAbort?.abort();
		this.autocompleteAbort = undefined;
	}

	private requireTextarea(): BoneTextareaNode {
		if (!this.textarea) throw new Error("OpenTUIComposer must be mounted first");
		return this.textarea;
	}

	private requireAutocomplete(): BoneSelectNode<AutocompleteItem> {
		if (!this.autocomplete) throw new Error("OpenTUIComposer must be mounted first");
		return this.autocomplete;
	}
}
