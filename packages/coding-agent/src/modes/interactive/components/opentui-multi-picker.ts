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
import type { ExtensionUISelectOption } from "../../../core/extensions/ui-v2.ts";
import { OPEN_TUI_COLORS } from "../opentui-design.ts";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";

export interface OpenTUIMultiPickerRequest<Value extends string> {
	title: string;
	options: readonly ExtensionUISelectOption<Value>[];
	initialValues: readonly Value[];
	searchable?: boolean;
	searchPlaceholder?: string;
}

function consume(event: KeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

/** Searchable multi-selection surface for bounded command configuration. */
export class OpenTUIMultiPicker<Value extends string> {
	readonly root: BoxRenderable;
	private readonly request: OpenTUIMultiPickerRequest<Value>;
	private readonly done: (values: Value[] | undefined) => void;
	private readonly selected: Set<Value>;
	private readonly search: InputRenderable | undefined;
	private readonly select: SelectRenderable;
	private readonly count: TextRenderable;
	private filteredOptions: ExtensionUISelectOption<Value>[];
	private completed = false;

	constructor(
		renderer: CliRenderer,
		request: OpenTUIMultiPickerRequest<Value>,
		done: (values: Value[] | undefined) => void,
	) {
		this.request = request;
		this.done = done;
		this.selected = new Set(request.initialValues);
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
			}),
		);
		if (request.searchable) {
			this.search = new InputRenderable(renderer, {
				width: "100%",
				placeholder: request.searchPlaceholder ?? "Search",
				textColor: OPEN_TUI_COLORS.text,
				focusedTextColor: OPEN_TUI_COLORS.text,
				placeholderColor: OPEN_TUI_COLORS.muted,
			});
			this.search.on(InputRenderableEvents.INPUT, (value: string) => this.filter(value));
			this.search.on(InputRenderableEvents.ENTER, () => this.toggleCurrent());
			this.root.add(this.search);
		}
		this.select = new SelectRenderable(renderer, {
			width: "100%",
			height: Math.min(10, Math.max(2, request.options.length * 2)),
			options: this.nativeOptions(),
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
		this.select.on(SelectRenderableEvents.ITEM_SELECTED, () => this.toggleCurrent());
		this.count = new TextRenderable(renderer, { content: "", fg: OPEN_TUI_COLORS.muted });
		this.root.add(this.select);
		this.root.add(this.count);
		this.root.add(
			new TextRenderable(renderer, {
				content: "Enter toggle · Up/Down move · Ctrl+S apply · Esc discard",
				fg: OPEN_TUI_COLORS.dim,
				truncate: true,
			}),
		);
		this.refreshCount();
	}

	focus(): void {
		(this.search ?? this.select).focus();
	}

	handleKey(event: KeyEvent): boolean {
		if (event.eventType === "release") return false;
		if (matchesOpenTUIAction(event, "cancel")) {
			this.finish(undefined);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "save")) {
			this.finish([...this.selected]);
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
			this.toggleCurrent();
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
		if (this.filteredOptions.length > 0) this.select.selectedIndex = 0;
	}

	private toggleCurrent(): void {
		const option = this.filteredOptions[this.select.getSelectedIndex()];
		if (!option || option.disabled) return;
		if (this.selected.has(option.value)) this.selected.delete(option.value);
		else this.selected.add(option.value);
		this.select.options = this.nativeOptions();
		this.refreshCount();
	}

	private finish(values: Value[] | undefined): void {
		if (this.completed) return;
		this.completed = true;
		this.done(values);
	}

	private refreshCount(): void {
		this.count.content = `${this.selected.size} selected`;
	}

	private nativeOptions(): Array<{ name: string; description: string }> {
		return this.filteredOptions.map((option) => ({
			name: `${this.selected.has(option.value) ? "[x]" : "[ ]"} ${option.label}`,
			description: option.description ?? "",
		}));
	}
}
