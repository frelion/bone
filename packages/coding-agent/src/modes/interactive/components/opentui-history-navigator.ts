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
import type { SessionEntry, SessionTreeNode } from "../../../core/session-manager.ts";
import { OPEN_TUI_COLORS } from "../opentui-design.ts";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";

export type OpenTUIHistoryNavigatorMode = "tree" | "fork";

interface HistoryItem {
	entry: SessionEntry;
	depth: number;
	label?: string;
}

function consume(event: KeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

function messageText(entry: SessionEntry): string {
	if (entry.type !== "message") return "";
	if (!("content" in entry.message)) return "";
	const content = entry.message.content;
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	return content
		.map((part) => (part.type === "text" && "text" in part ? part.text : ""))
		.filter(Boolean)
		.join("\n");
}

function entryContent(entry: SessionEntry): string {
	if (entry.type === "message") return messageText(entry) || "Empty message";
	if (entry.type === "branch_summary" || entry.type === "compaction") return entry.summary;
	if (entry.type === "custom_message") {
		return typeof entry.content === "string"
			? entry.content
			: entry.content.map((part) => (part.type === "text" ? part.text : "")).join("\n");
	}
	if (entry.type === "plan_proposal") return entry.proposal.content;
	if (entry.type === "plan_decision") return `Plan v${entry.version}: ${entry.decision}`;
	if (entry.type === "model_change") return `${entry.provider}/${entry.modelId}`;
	if (entry.type === "thinking_level_change") return entry.thinkingLevel;
	if (entry.type === "collaboration_mode_change") return entry.mode;
	if (entry.type === "question_asked") return entry.request.questions.map((question) => question.question).join("\n");
	if (entry.type === "question_answered") return `${entry.answers.length} answers`;
	if (entry.type === "question_cancelled") return entry.reason;
	return entry.type.replaceAll("_", " ");
}

function entryRole(entry: SessionEntry): string {
	if (entry.type === "message") return entry.message.role;
	return entry.type.replaceAll("_", " ");
}

function flattenTree(nodes: readonly SessionTreeNode[], depth = 0): HistoryItem[] {
	const items: HistoryItem[] = [];
	for (const node of nodes) {
		items.push({ entry: node.entry, depth, label: node.label });
		items.push(...flattenTree(node.children, depth + 1));
	}
	return items;
}

/** Searchable, full-main-area conversation history navigator shared by /tree and /fork. */
export class OpenTUIHistoryNavigator {
	readonly root: BoxRenderable;
	private readonly mode: OpenTUIHistoryNavigatorMode;
	private readonly done: (entryId: string | undefined) => void;
	private readonly currentLeafId: string | undefined;
	private readonly allItems: HistoryItem[];
	private filteredItems: HistoryItem[];
	private readonly search: InputRenderable;
	private readonly select: SelectRenderable;
	private readonly previewTitle: TextRenderable;
	private readonly preview: TextRenderable;
	private completed = false;

	constructor(
		renderer: CliRenderer,
		options: {
			mode: OpenTUIHistoryNavigatorMode;
			tree: readonly SessionTreeNode[];
			currentLeafId?: string;
			onDone: (entryId: string | undefined) => void;
		},
	) {
		this.mode = options.mode;
		this.done = options.onDone;
		this.currentLeafId = options.currentLeafId;
		this.allItems = flattenTree(options.tree);
		this.filteredItems = [...this.allItems];
		this.root = new BoxRenderable(renderer, {
			width: "100%",
			height: "100%",
			minHeight: 0,
			flexDirection: "column",
			paddingX: 1,
			paddingY: 1,
			backgroundColor: OPEN_TUI_COLORS.page,
		});
		this.root.add(
			new TextRenderable(renderer, {
				content: options.mode === "fork" ? "Fork conversation" : "Conversation tree",
				fg: OPEN_TUI_COLORS.primary,
				attributes: TextAttributes.BOLD,
			}),
		);
		this.search = new InputRenderable(renderer, {
			width: "100%",
			placeholder: "Search history",
			textColor: OPEN_TUI_COLORS.text,
			focusedTextColor: OPEN_TUI_COLORS.text,
			placeholderColor: OPEN_TUI_COLORS.muted,
		});
		this.search.on(InputRenderableEvents.INPUT, (value: string) => this.filter(value));
		this.search.on(InputRenderableEvents.ENTER, () => this.choose());
		this.root.add(this.search);
		const body = new BoxRenderable(renderer, {
			width: "100%",
			flexGrow: 1,
			minHeight: 0,
			flexDirection: "column",
		});
		this.select = new SelectRenderable(renderer, {
			width: "100%",
			flexGrow: 1,
			minHeight: 4,
			options: this.nativeOptions(),
			selectedIndex: Math.max(
				0,
				this.allItems.findIndex((item) => item.entry.id === options.currentLeafId),
			),
			showDescription: true,
			showSelectionIndicator: true,
			wrapSelection: true,
			backgroundColor: OPEN_TUI_COLORS.page,
			textColor: OPEN_TUI_COLORS.text,
			focusedTextColor: OPEN_TUI_COLORS.text,
			selectedBackgroundColor: OPEN_TUI_COLORS.selection,
			selectedTextColor: OPEN_TUI_COLORS.selectionText,
			descriptionColor: OPEN_TUI_COLORS.muted,
			selectedDescriptionColor: OPEN_TUI_COLORS.muted,
		});
		this.select.on(SelectRenderableEvents.SELECTION_CHANGED, () => this.refreshPreview());
		this.select.on(SelectRenderableEvents.ITEM_SELECTED, () => this.choose());
		body.add(this.select);
		this.previewTitle = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.primary,
			attributes: TextAttributes.BOLD,
			truncate: true,
		});
		this.preview = new TextRenderable(renderer, {
			content: "",
			fg: OPEN_TUI_COLORS.text,
			wrapMode: "word",
			maxHeight: 6,
		});
		body.add(this.previewTitle);
		body.add(this.preview);
		this.root.add(body);
		this.root.add(
			new TextRenderable(renderer, {
				content: "Type to filter · Up/Down move · Enter choose · Esc return",
				fg: OPEN_TUI_COLORS.dim,
				truncate: true,
			}),
		);
		this.refreshPreview();
	}

	focus(): void {
		this.search.focus();
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
			this.choose();
			return consume(event);
		}
		return false;
	}

	private filter(query: string): void {
		const normalized = query.trim().toLowerCase();
		this.filteredItems = normalized
			? this.allItems.filter((item) =>
					`${item.label ?? ""} ${entryRole(item.entry)} ${entryContent(item.entry)}`
						.toLowerCase()
						.includes(normalized),
				)
			: [...this.allItems];
		this.select.options = this.nativeOptions();
		if (this.filteredItems.length > 0) this.select.selectedIndex = 0;
		this.select.visible = this.filteredItems.length > 0;
		this.refreshPreview();
	}

	private choose(): void {
		const item = this.filteredItems[this.select.getSelectedIndex()];
		if (!item || (this.mode === "fork" && !this.isForkable(item.entry))) return;
		this.finish(item.entry.id);
	}

	private finish(entryId: string | undefined): void {
		if (this.completed) return;
		this.completed = true;
		this.done(entryId);
	}

	private refreshPreview(): void {
		const item = this.filteredItems[this.select.getSelectedIndex()];
		this.previewTitle.content = item
			? `${item.label ? `${item.label} · ` : ""}${entryRole(item.entry)}`
			: "No matches";
		this.preview.content = item ? entryContent(item.entry) : "Try a different search.";
	}

	private nativeOptions(): Array<{ name: string; description: string }> {
		return this.filteredItems.map((item) => {
			const forkable = this.isForkable(item.entry);
			const prefix = `${"  ".repeat(item.depth)}${item.depth ? "|- " : ""}`;
			const current = item.entry.id === this.currentLeafId ? " (current)" : "";
			const unavailable = this.mode === "fork" && !forkable ? " (not forkable)" : "";
			return {
				name: `${prefix}${item.label ? `[${item.label}] ` : ""}${entryRole(item.entry)}${current}${unavailable}`,
				description: entryContent(item.entry).replace(/\s+/g, " ").trim().slice(0, 120),
			};
		});
	}

	private isForkable(entry: SessionEntry): boolean {
		return entry.type === "message" && entry.message.role === "user";
	}
}
