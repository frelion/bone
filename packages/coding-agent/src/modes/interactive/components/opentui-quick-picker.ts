import {
	BoxRenderable,
	type CliRenderer,
	InputRenderable,
	InputRenderableEvents,
	type KeyEvent,
	SelectRenderable,
	SelectRenderableEvents,
	TextAttributes,
	TextRenderable,
} from "@opentui/core";
import type { ExtensionUISelectOption, ExtensionUISelectRequest } from "../../../core/extensions/ui-v2.ts";
import { OPEN_TUI_COLORS } from "../opentui-design.ts";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";

export interface OpenTUIQuickPickerRequest<Value extends string> extends ExtensionUISelectRequest<Value> {
	searchable?: boolean;
	searchPlaceholder?: string;
}

function consume(event: KeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

/** Lightweight command picker mounted above the composer without obscuring conversation context. */
export class OpenTUIQuickPicker<Value extends string> {
	readonly root: BoxRenderable;
	private readonly request: OpenTUIQuickPickerRequest<Value>;
	private readonly done: (value: Value | undefined) => void;
	private readonly searchInput: InputRenderable | undefined;
	private readonly select: SelectRenderable;
	private readonly emptyNode: TextRenderable;
	private filteredOptions: ExtensionUISelectOption<Value>[];
	private completed = false;

	constructor(
		renderer: CliRenderer,
		request: OpenTUIQuickPickerRequest<Value>,
		done: (value: Value | undefined) => void,
	) {
		this.request = request;
		this.done = done;
		this.filteredOptions = [...request.options];
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
				content: request.title,
				fg: OPEN_TUI_COLORS.primary,
				attributes: TextAttributes.BOLD,
				truncate: true,
			}),
		);
		if (request.searchable) {
			this.searchInput = new InputRenderable(renderer, {
				width: "100%",
				placeholder: request.searchPlaceholder ?? "Search",
				textColor: OPEN_TUI_COLORS.text,
				focusedTextColor: OPEN_TUI_COLORS.text,
				placeholderColor: OPEN_TUI_COLORS.muted,
			});
			this.searchInput.on(InputRenderableEvents.INPUT, (value: string) => this.filter(value));
			this.searchInput.on(InputRenderableEvents.ENTER, () => this.chooseCurrent());
			this.root.add(this.searchInput);
		}
		const initialIndex = Math.max(
			0,
			request.initialValue === undefined
				? 0
				: request.options.findIndex((option) => option.value === request.initialValue),
		);
		this.select = new SelectRenderable(renderer, {
			width: "100%",
			height: Math.min(10, Math.max(2, request.options.length * 2)),
			options: this.nativeOptions(),
			selectedIndex: initialIndex,
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
		this.emptyNode = new TextRenderable(renderer, {
			content: "No matching options",
			fg: OPEN_TUI_COLORS.muted,
		});
		this.emptyNode.visible = false;
		this.root.add(this.select);
		this.root.add(this.emptyNode);
		this.root.add(
			new TextRenderable(renderer, {
				content: request.searchable
					? "Type to filter · Up/Down move · Enter choose · Esc close"
					: "Up/Down move · Enter choose · Esc close",
				fg: OPEN_TUI_COLORS.dim,
				truncate: true,
			}),
		);
	}

	focus(): void {
		(this.searchInput ?? this.select).focus();
	}

	handleKey(event: KeyEvent): boolean {
		if (event.eventType === "release") return false;
		if (matchesOpenTUIAction(event, "cancel")) {
			this.finish(undefined);
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
		if (matchesOpenTUIAction(event, "confirm")) {
			this.chooseCurrent();
			return consume(event);
		}
		return false;
	}

	private filter(query: string): void {
		const normalized = query.trim().toLowerCase();
		this.filteredOptions = normalized
			? this.request.options.filter((option) =>
					`${option.label} ${option.description ?? ""} ${option.value}`.toLowerCase().includes(normalized),
				)
			: [...this.request.options];
		this.select.options = this.nativeOptions();
		this.select.visible = this.filteredOptions.length > 0;
		this.emptyNode.visible = this.filteredOptions.length === 0;
		if (this.filteredOptions.length > 0) this.select.selectedIndex = 0;
	}

	private chooseCurrent(): void {
		const option = this.filteredOptions[this.select.getSelectedIndex()];
		if (option && !option.disabled) this.finish(option.value);
	}

	private finish(value: Value | undefined): void {
		if (this.completed) return;
		this.completed = true;
		this.done(value);
	}

	private nativeOptions(): Array<{ name: string; description: string }> {
		return this.filteredOptions.map((option) => ({
			name: option.disabled ? `${option.label} (unavailable)` : option.label,
			description: option.description ?? "",
		}));
	}
}
