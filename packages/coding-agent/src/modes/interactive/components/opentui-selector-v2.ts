import type {
	BoneContainerNode,
	BoneInputNode,
	BoneNode,
	BoneRenderContext,
	BoneScrollViewNode,
	BoneView,
} from "@frelion/bone-tui";
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

/** Generic structured selector. Callers map keyboard events to handleAction(). */
export class OpenTUISelectorViewV2<T> implements BoneView {
	private readonly options: OpenTUISelectorOptions<T>;
	private readonly selectorTheme: Theme;
	private allItems: OpenTUISelectorItem<T>[];
	private filteredItems: OpenTUISelectorItem<T>[];
	private selectedIndex: number;
	private queryValue = "";
	private context: BoneRenderContext | undefined;
	private dialog: OpenTUIDialogMount | undefined;
	private list: BoneScrollViewNode | undefined;
	private searchInput: BoneInputNode | undefined;

	constructor(options: OpenTUISelectorOptions<T>) {
		this.options = options;
		this.selectorTheme = options.theme ?? theme;
		this.allItems = [...options.items];
		this.filteredItems = [...options.items];
		this.selectedIndex = Math.max(0, Math.min(options.selectedIndex ?? 0, this.filteredItems.length - 1));
	}

	get selectedItem(): OpenTUISelectorItem<T> | undefined {
		return this.filteredItems[this.selectedIndex];
	}

	get query(): string {
		return this.queryValue;
	}

	mount(context: BoneRenderContext): BoneNode {
		if (this.dialog) throw new Error("OpenTUISelectorViewV2 is already mounted");
		this.context = context;
		this.dialog = createOpenTUIDialogShell(context, {
			title: this.options.title,
			subtitle: this.options.subtitle,
			footer: this.options.footer ?? "↑↓ navigate · Enter select · Esc cancel",
			theme: this.selectorTheme,
		});
		if (this.options.searchable) {
			this.searchInput = context.createInput({
				width: "100%",
				placeholder: this.options.searchPlaceholder ?? "Search",
				textColor: this.selectorTheme.getFgColor("text"),
				focusedTextColor: this.selectorTheme.getFgColor("text"),
				placeholderColor: this.selectorTheme.getFgColor("muted"),
				onInput: (value) => this.setQuery(value),
			});
			this.dialog.body.append(this.searchInput);
			this.dialog.body.append(context.createSpacer({ size: 1, direction: "vertical" }));
		}
		this.list = context.createScrollView({ width: "100%", flexGrow: 1, minHeight: 1, scrollY: true });
		this.dialog.body.append(this.list);
		this.rebuildList();
		(this.searchInput ?? this.dialog.root).focus();
		return this.dialog.root;
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
			const selected = this.selectedItem;
			if (selected && !selected.disabled) this.options.onSelect(selected.value);
			return true;
		}
		if (this.filteredItems.length === 0) return true;
		const pageSize = Math.max(1, this.options.pageSize ?? 8);
		const delta = action === "up" ? -1 : action === "down" ? 1 : action === "pageUp" ? -pageSize : pageSize;
		this.selectedIndex = Math.max(0, Math.min(this.filteredItems.length - 1, this.selectedIndex + delta));
		this.rebuildList();
		const selected = this.selectedItem;
		if (selected) this.options.onPreview?.(selected.value);
		return true;
	}

	private filterItems(): void {
		const query = this.queryValue.trim().toLowerCase();
		this.filteredItems = query
			? this.allItems.filter((item) =>
					`${item.label} ${item.description ?? ""} ${item.keywords ?? ""}`.toLowerCase().includes(query),
				)
			: [...this.allItems];
		this.selectedIndex = Math.max(0, Math.min(this.selectedIndex, this.filteredItems.length - 1));
		this.rebuildList();
	}

	private rebuildList(): void {
		const context = this.context;
		const list = this.list;
		if (!context || !list) return;
		list.clear();
		if (this.filteredItems.length === 0) {
			list.append(
				context.createText({ content: "No matching options", fg: this.selectorTheme.getFgColor("muted") }),
			);
			return;
		}
		for (let index = 0; index < this.filteredItems.length; index++) {
			const item = this.filteredItems[index]!;
			const selected = index === this.selectedIndex;
			const row: BoneContainerNode = context.createBox({
				width: "100%",
				flexDirection: "row",
				gap: 1,
				backgroundColor: selected ? this.selectorTheme.getBgColor("selectedBg") : undefined,
				onMouseDown: (event) => {
					this.selectedIndex = index;
					this.rebuildList();
					this.options.onPreview?.(item.value);
					event.preventDefault();
					event.stopPropagation();
				},
			});
			row.append(
				context.createText({
					content: selected ? "›" : " ",
					fg: this.selectorTheme.getFgColor(selected ? "accent" : "muted"),
					flexShrink: 0,
				}),
			);
			row.append(
				context.createText({
					content: item.label,
					fg: this.selectorTheme.getFgColor(item.disabled ? "dim" : selected ? "accent" : "text"),
					bold: selected,
					truncate: true,
					flexGrow: 1,
					minWidth: 1,
				}),
			);
			if (item.description || item.current) {
				row.append(
					context.createText({
						content: item.current ? `${item.description ?? ""} (current)`.trim() : item.description!,
						fg: this.selectorTheme.getFgColor(item.current ? "success" : "muted"),
						truncate: true,
						flexShrink: 1,
					}),
				);
			}
			list.append(row);
		}
		const selected = this.selectedItem;
		if (selected) list.scrollTo(Math.max(0, this.selectedIndex - 1));
	}
}
