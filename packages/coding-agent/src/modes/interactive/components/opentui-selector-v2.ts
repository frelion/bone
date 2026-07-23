import {
	BoxRenderable,
	type CliRenderer,
	InputRenderable,
	InputRenderableEvents,
	type Renderable,
	SelectRenderable,
	SelectRenderableEvents,
} from "@opentui/core";
import { type Theme, theme } from "../theme/theme.ts";
import { createOpenTUIDialogShell, type OpenTUIDialogMount } from "./opentui-dialog-v2.ts";

export type OpenTUISelectorAction = "confirm" | "cancel" | "up" | "down" | "pageUp" | "pageDown";

export interface OpenTUISelectorItem<T> {
	value: T;
	label: string;
	description?: string;
	keywords?: string;
	current?: boolean;
	disabled?: boolean;
}

export interface OpenTUISelectorOptions<T> {
	title: string;
	subtitle?: string;
	items: readonly OpenTUISelectorItem<T>[];
	selectedIndex?: number;
	searchable?: boolean;
	searchPlaceholder?: string;
	pageSize?: number;
	footer?: string;
	theme?: Theme;
	onSelect: (value: T) => void;
	onCancel: () => void;
	onPreview?: (value: T) => void;
}

/** Native SelectRenderable-backed structured selector. */
export class OpenTUISelectorViewV2<T> {
	private readonly options: OpenTUISelectorOptions<T>;
	private readonly selectorTheme: Theme;
	private allItems: OpenTUISelectorItem<T>[];
	private filteredItems: OpenTUISelectorItem<T>[];
	private selectedIndex: number;
	private queryValue = "";
	private dialog: OpenTUIDialogMount | undefined;
	private select: SelectRenderable | undefined;
	private searchInput: InputRenderable | undefined;

	constructor(options: OpenTUISelectorOptions<T>) {
		this.options = options;
		this.selectorTheme = options.theme ?? theme;
		this.allItems = [...options.items];
		this.filteredItems = [...options.items];
		this.selectedIndex = Math.max(0, Math.min(options.selectedIndex ?? 0, this.filteredItems.length - 1));
	}

	get root(): BoxRenderable | undefined {
		return this.dialog?.root;
	}

	get focusTarget(): Renderable | undefined {
		return this.searchInput ?? this.select;
	}

	get selectedItem(): OpenTUISelectorItem<T> | undefined {
		const index = this.select?.getSelectedIndex() ?? this.selectedIndex;
		return this.filteredItems[index];
	}

	get query(): string {
		return this.queryValue;
	}

	build(renderer: CliRenderer): BoxRenderable {
		if (this.dialog) throw new Error("OpenTUISelectorViewV2 is already built");
		this.dialog = createOpenTUIDialogShell(renderer, {
			title: this.options.title,
			subtitle: this.options.subtitle,
			footer: this.options.footer,
			theme: this.selectorTheme,
		});
		if (this.options.searchable) {
			this.searchInput = new InputRenderable(renderer, {
				width: "100%",
				placeholder: this.options.searchPlaceholder ?? "Search",
				textColor: this.selectorTheme.getFgColor("text"),
				focusedTextColor: this.selectorTheme.getFgColor("text"),
				placeholderColor: this.selectorTheme.getFgColor("muted"),
			});
			this.searchInput.on(InputRenderableEvents.INPUT, (value: string) => this.setQuery(value));
			this.searchInput.on(InputRenderableEvents.ENTER, () => this.selectCurrent());
			this.dialog.body.add(this.searchInput);
			this.dialog.body.add(new BoxRenderable(renderer, { height: 1, flexShrink: 0 }));
		}
		this.select = new SelectRenderable(renderer, {
			width: "100%",
			flexGrow: 1,
			minHeight: 1,
			options: this.nativeOptions(),
			selectedIndex: this.selectedIndex,
			backgroundColor: this.selectorTheme.getBgColor("customMessageBg"),
			textColor: this.selectorTheme.getFgColor("text"),
			focusedTextColor: this.selectorTheme.getFgColor("text"),
			selectedBackgroundColor: this.selectorTheme.getBgColor("selectedBg"),
			selectedTextColor: this.selectorTheme.getFgColor("accent"),
			descriptionColor: this.selectorTheme.getFgColor("muted"),
			selectedDescriptionColor: this.selectorTheme.getFgColor("muted"),
			showDescription: true,
			showSelectionIndicator: true,
		});
		this.select.on(SelectRenderableEvents.SELECTION_CHANGED, (index: number) => {
			this.selectedIndex = index;
			const selected = this.filteredItems[index];
			if (selected) this.options.onPreview?.(selected.value);
		});
		this.select.on(SelectRenderableEvents.ITEM_SELECTED, (index: number) => {
			const selected = this.filteredItems[index];
			if (selected && !selected.disabled) this.options.onSelect(selected.value);
		});
		this.dialog.body.add(this.select);
		return this.dialog.root;
	}

	focus(): void {
		this.focusTarget?.focus();
	}

	setItems(items: readonly OpenTUISelectorItem<T>[]): void {
		this.allItems = [...items];
		this.filterItems();
	}

	setStatus(message: string | undefined): void {
		if (!this.dialog) return;
		this.dialog.status.content = message ?? "";
		this.dialog.status.visible = Boolean(message);
	}

	setQuery(query: string): void {
		this.queryValue = query;
		this.filterItems();
	}

	handleAction(action: OpenTUISelectorAction): boolean {
		if (action === "cancel") {
			this.options.onCancel();
			return true;
		}
		if (action === "confirm") {
			this.selectCurrent();
			return true;
		}
		if (!this.select || this.filteredItems.length === 0) return true;
		const steps = action === "pageUp" || action === "pageDown" ? Math.max(1, this.options.pageSize ?? 8) : 1;
		if (action === "up" || action === "pageUp") this.select.moveUp(steps);
		else this.select.moveDown(steps);
		return true;
	}

	private selectCurrent(): void {
		this.select?.selectCurrent();
	}

	private filterItems(): void {
		const query = this.queryValue.trim().toLowerCase();
		this.filteredItems = query
			? this.allItems.filter((item) =>
					`${item.label} ${item.description ?? ""} ${item.keywords ?? ""}`.toLowerCase().includes(query),
				)
			: [...this.allItems];
		this.selectedIndex = Math.max(0, Math.min(this.selectedIndex, this.filteredItems.length - 1));
		if (!this.select) return;
		this.select.options = this.nativeOptions();
		if (this.filteredItems.length > 0) this.select.selectedIndex = this.selectedIndex;
	}

	private nativeOptions(): Array<{ name: string; description: string }> {
		return this.filteredItems.map((item) => ({
			name: item.label,
			description: item.current ? `${item.description ?? ""} (current)`.trim() : (item.description ?? ""),
		}));
	}
}
