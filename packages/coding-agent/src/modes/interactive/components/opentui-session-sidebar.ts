import {
	BoxRenderable,
	type CliRenderer,
	type KeyEvent,
	type MouseEvent,
	ScrollBoxRenderable,
	TextAttributes,
	TextareaRenderable,
	TextRenderable,
} from "@opentui/core";
import stringWidth from "string-width";
import type { InteractiveSessionSummary } from "../../../core/interactive-session-host.ts";
import type { MemorySearchResult } from "../../../core/memory.ts";
import { OPEN_TUI_COLORS, OPEN_TUI_LAYOUT } from "../opentui-design.ts";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";
import { type Theme, type ThemeColor, theme } from "../theme/theme.ts";
import { formatConversationActivityTime, formatConversationCreatedTime } from "./conversation-time.ts";

const graphemeSegmenter = new Intl.Segmenter(undefined, { granularity: "grapheme" });

const STATE_ICON: Record<InteractiveSessionSummary["state"], string> = {
	foreground: "●",
	"background-running": "↻",
	"background-waiting": "!",
	cold: "○",
};

type SidebarItemState =
	| { kind: "normal" }
	| { kind: "confirm-delete"; path: string }
	| { kind: "status"; path: string; tone: "success" | "error"; message: string };

function normalizePreview(text: string): string {
	return text
		.replace(/[\r\n\t]+/g, " ")
		.replace(/\s+/g, " ")
		.trim();
}

function clearChildren(root: BoxRenderable): void {
	for (const child of root.getChildren()) child.destroyRecursively();
}

/** Keep a running conversation's preview anchored to its newest model output. */
function latestPreviewWindow(text: string, width: number): string {
	if (width <= 0 || stringWidth(text) <= width) return text;
	const graphemes = Array.from(graphemeSegmenter.segment(text), ({ segment }) => segment);
	const tail: string[] = [];
	let usedWidth = 0;
	for (let index = graphemes.length - 1; index >= 0; index--) {
		const grapheme = graphemes[index]!;
		const graphemeWidth = stringWidth(grapheme);
		if (usedWidth + graphemeWidth > width) break;
		tail.unshift(grapheme);
		usedWidth += graphemeWidth;
	}
	return tail.join("");
}

function stateColor(state: InteractiveSessionSummary["state"]): ThemeColor {
	if (state === "foreground") return "accent";
	if (state === "background-running") return "warning";
	if (state === "background-waiting") return "error";
	return "muted";
}

function consume(event: KeyEvent): true {
	event.preventDefault();
	event.stopPropagation();
	return true;
}

function sortByActivity(sessions: readonly InteractiveSessionSummary[]): InteractiveSessionSummary[] {
	return sessions
		.map((session, index) => ({ session, index }))
		.sort(
			(left, right) =>
				right.session.modified.getTime() - left.session.modified.getTime() || left.index - right.index,
		)
		.map(({ session }) => session);
}

function formatThroughput(tokensPerSecond: number | undefined): string {
	if (tokensPerSecond === undefined || !Number.isFinite(tokensPerSecond)) return "";
	return `${tokensPerSecond.toFixed(1)} tok/s`;
}

interface MarqueeEntry {
	node: TextRenderable;
	text: string;
}

interface SessionRowRecord {
	row: BoxRenderable;
	stateNode: TextRenderable;
	titleNode: TextRenderable;
	previewNode: TextRenderable;
	throughputNode: TextRenderable | undefined;
	metadataNode: TextRenderable;
}

/** Structured OpenTUI conversation sidebar with search and session actions. */
export class OpenTUISessionSidebar {
	readonly root: BoxRenderable;
	readonly list: ScrollBoxRenderable;
	private sidebarTheme: Theme;
	private sessions: InteractiveSessionSummary[] = [];
	private selectedIndex = 0;
	private selectedPath: string | undefined;
	private itemState: SidebarItemState = { kind: "normal" };
	private searchResults: readonly MemorySearchResult[] | undefined;
	private searchStatus: string | undefined;
	private searchQueryValue = "";
	private searchMode = false;
	private searchStartPath: string | undefined;
	private previewedSearchPath: string | undefined;
	private readonly renderer: CliRenderer;
	private searchInput: TextareaRenderable | undefined;
	private searchStatusNode: TextRenderable | undefined;
	private focused = false;
	private frozenOrder: string[] | undefined;
	private readonly marqueeEntries = new Map<string, MarqueeEntry>();
	private readonly rowRecords = new Map<string, SessionRowRecord>();
	private renderedPaths: string[] = [];
	private marqueeTimer: ReturnType<typeof setInterval> | undefined;

	public onActivateSession?: (sessionPath: string) => void;
	public onPreviewSession?: (sessionPath: string) => void;
	public onDeleteSession?: (sessionPath: string, replacementPath: string | undefined) => void;
	public onSearchQueryChange?: (query: string) => void;
	public onSearchStateChange?: (active: boolean) => void;
	public onFocusChat?: () => void;
	public onFocusRequest?: () => void;
	public onScrollChat?: (direction: "up" | "down") => void;
	public onLoadMore?: () => void;
	public onInterrupt?: () => void;
	public onExit?: () => void;

	constructor(renderer: CliRenderer, sidebarTheme: Theme = theme) {
		this.renderer = renderer;
		this.sidebarTheme = sidebarTheme;
		this.root = new BoxRenderable(renderer, {
			width: "100%",
			height: "100%",
			flexDirection: "column",
			focusable: true,
			onMouseDown: () => this.onFocusRequest?.(),
		});
		this.list = new ScrollBoxRenderable(renderer, { width: "100%", flexGrow: 1, minHeight: 0, scrollY: true });
		this.rebuildChrome();
		this.startMarquee();
	}

	get searchActive(): boolean {
		return this.searchMode;
	}

	get searchQuery(): string | undefined {
		return this.searchActive ? this.searchQueryValue : undefined;
	}

	get focusTarget(): BoxRenderable | TextareaRenderable {
		return this.searchInput ?? this.root;
	}

	setFocused(focused: boolean): void {
		if (focused === this.focused) {
			return;
		}
		if (focused) {
			this.sessions = sortByActivity(this.sessions);
			this.frozenOrder = this.sessions.map((session) => session.path);
		} else {
			this.frozenOrder = undefined;
			this.sessions = sortByActivity(this.sessions);
		}
		this.focused = focused;
		this.reconcileSelection();
		this.rebuildList();
	}

	updateTheme(nextTheme: Theme): void {
		this.sidebarTheme = nextTheme;
		this.rebuildChrome();
	}

	setSessions(sessions: InteractiveSessionSummary[]): void {
		if (this.focused && this.frozenOrder) {
			const sessionsByPath = new Map(sessions.map((session) => [session.path, session]));
			const frozen = this.frozenOrder.flatMap((path) => {
				const session = sessionsByPath.get(path);
				if (!session) return [];
				sessionsByPath.delete(path);
				return [session];
			});
			const additions = sortByActivity([...sessionsByPath.values()]);
			this.sessions = [...frozen, ...additions];
			this.frozenOrder = this.sessions.map((session) => session.path);
		} else {
			this.sessions = sortByActivity(sessions);
		}
		this.reconcileSelection();
		const itemState = this.itemState;
		if (itemState.kind !== "normal" && !sessions.some((session) => session.path === itemState.path)) {
			this.itemState = { kind: "normal" };
		}
		this.rebuildList();
	}

	setSearchResults(results: readonly MemorySearchResult[] | undefined): void {
		if (!this.searchActive) return;
		this.searchResults = results;
		this.reconcileSelection();
		this.rebuildList();
	}

	setSearchStatus(status: string | undefined): void {
		this.searchStatus = status;
		if (this.searchStatusNode) {
			this.searchStatusNode.content = status ?? "";
			this.searchStatusNode.visible = Boolean(status);
		}
	}

	setStatusMessage(message: string | undefined, tone: "success" | "error" = "success"): void {
		const selected = this.getDisplayedSessions()[this.selectedIndex];
		this.itemState =
			message && selected ? { kind: "status", path: selected.path, tone, message } : { kind: "normal" };
		this.rebuildList();
	}

	dispose(): void {
		if (this.marqueeTimer) clearInterval(this.marqueeTimer);
		this.marqueeTimer = undefined;
		this.marqueeEntries.clear();
	}

	startSearch(): void {
		if (this.searchActive) return;
		this.searchStartPath = this.selectedPath;
		this.searchMode = true;
		this.previewedSearchPath = undefined;
		this.itemState = { kind: "normal" };
		this.searchResults = undefined;
		this.searchStatus = undefined;
		this.rebuildChrome();
		this.onSearchQueryChange?.(this.searchQueryValue);
		this.onSearchStateChange?.(true);
	}

	stopSearch(options?: { restorePreview?: boolean }): void {
		if (!this.searchActive) return;
		const restorePath =
			options?.restorePreview && this.previewedSearchPath && this.previewedSearchPath !== this.searchStartPath
				? this.searchStartPath
				: undefined;
		this.searchResults = undefined;
		this.searchMode = false;
		this.searchStatus = undefined;
		this.searchStartPath = undefined;
		this.previewedSearchPath = undefined;
		if (restorePath) this.selectedPath = restorePath;
		this.searchInput = undefined;
		this.reconcileSelection();
		this.rebuildChrome();
		this.onSearchQueryChange?.("");
		this.onSearchStateChange?.(false);
		if (restorePath) this.onPreviewSession?.(restorePath);
	}

	handleKey(event: KeyEvent): boolean {
		if (event.eventType === "release") return false;
		if (this.itemState.kind === "confirm-delete") {
			if (matchesOpenTUIAction(event, "sidebarDelete")) {
				const sessionPath = this.itemState.path;
				this.itemState = { kind: "normal" };
				const selectedIndex = this.sessions.findIndex((session) => session.path === sessionPath);
				const replacementPath =
					this.sessions[selectedIndex + 1]?.path ?? this.sessions[Math.max(0, selectedIndex - 1)]?.path;
				this.onDeleteSession?.(sessionPath, replacementPath === sessionPath ? undefined : replacementPath);
				this.rebuildList();
				return consume(event);
			}
			if (matchesOpenTUIAction(event, "cancel")) {
				this.itemState = { kind: "normal" };
				this.rebuildList();
				return consume(event);
			}
			return consume(event);
		}

		if (this.itemState.kind === "status") {
			this.itemState = { kind: "normal" };
			this.rebuildList();
		}
		if (matchesOpenTUIAction(event, "clear")) {
			this.onInterrupt?.();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "exit")) {
			this.onExit?.();
			return consume(event);
		}
		if (this.searchActive) return this.handleSearchKey(event);
		if (matchesOpenTUIAction(event, "focusRight")) {
			this.onFocusChat?.();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "confirm")) {
			const selected = this.getDisplayedSessions()[this.selectedIndex];
			if (selected) this.activateSession(selected.path);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "up")) {
			this.moveSelection(-1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "down")) {
			this.moveSelection(1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "pageUp")) {
			this.onScrollChat?.("up");
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "pageDown")) {
			this.onScrollChat?.("down");
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "sidebarSearch")) {
			this.startSearch();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "sidebarDelete")) {
			const selected = this.getDisplayedSessions()[this.selectedIndex];
			if (selected) {
				this.itemState = { kind: "confirm-delete", path: selected.path };
				this.rebuildList();
			}
			return consume(event);
		}
		return false;
	}

	private handleSearchKey(event: KeyEvent): boolean {
		if (matchesOpenTUIAction(event, "focusRight")) {
			this.stopSearch();
			this.onFocusChat?.();
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "cancel")) {
			this.stopSearch({ restorePreview: true });
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "confirm")) {
			const selected = this.getDisplayedSessions()[this.selectedIndex];
			if (selected) {
				this.stopSearch();
				this.activateSession(selected.path);
			}
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "up")) {
			this.moveSearchSelection(-1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "down")) {
			this.moveSearchSelection(1);
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "pageUp")) {
			this.onScrollChat?.("up");
			return consume(event);
		}
		if (matchesOpenTUIAction(event, "pageDown")) {
			this.onScrollChat?.("down");
			return consume(event);
		}
		return false;
	}

	private activateSession(sessionPath: string): void {
		this.onFocusChat?.();
		this.onActivateSession?.(sessionPath);
	}

	private rebuildChrome(): void {
		for (const child of this.root.getChildren()) {
			if (child === this.list) {
				this.root.remove(child);
				continue;
			}
			child.destroyRecursively();
		}
		const header = new BoxRenderable(this.renderer, {
			width: "100%",
			height: 2,
			flexDirection: "column",
			alignItems: "center",
			justifyContent: "center",
		});
		header.add(
			new TextRenderable(this.renderer, {
				content: "CONVERSATIONS",
				fg: OPEN_TUI_COLORS.muted,
				attributes: TextAttributes.BOLD,
				truncate: true,
				width: "100%",
			}),
		);
		this.root.add(header);

		if (this.searchStartPath !== undefined) {
			this.searchInput = new TextareaRenderable(this.renderer, {
				width: "100%",
				height: 1,
				initialValue: this.searchQueryValue,
				placeholder: "Search conversations",
				textColor: this.sidebarTheme.getFgColor("text"),
				focusedTextColor: this.sidebarTheme.getFgColor("text"),
				placeholderColor: this.sidebarTheme.getFgColor("muted"),
				onContentChange: () => {
					this.searchQueryValue = this.searchInput?.plainText ?? "";
					this.searchResults = undefined;
					this.reconcileSelection();
					this.rebuildList();
					this.onSearchQueryChange?.(this.searchQueryValue);
				},
			});
			this.root.add(this.searchInput);
			this.searchStatusNode = new TextRenderable(this.renderer, {
				content: this.searchStatus ?? "",
				fg: this.sidebarTheme.getFgColor("muted"),
				wrapMode: "word",
			});
			this.searchStatusNode.visible = Boolean(this.searchStatus);
			this.root.add(this.searchStatusNode);
		} else {
			this.searchInput = undefined;
			this.searchStatusNode = undefined;
		}

		this.root.add(this.list);
		const shortcutFooter = new BoxRenderable(this.renderer, {
			width: "100%",
			height: 3,
			flexDirection: "column",
			flexShrink: 0,
		});
		shortcutFooter.add(
			new TextRenderable(this.renderer, {
				content: "─".repeat(OPEN_TUI_LAYOUT.sidebarMaxWidth),
				height: 1,
				fg: OPEN_TUI_COLORS.borderSubtle,
				truncate: true,
			}),
		);
		const shortcutLines = this.searchActive
			? ["↑↓ results   type to search", "↵ open       esc cancel"]
			: ["↑↓ select    / search", "d delete     ↵ open"];
		for (const content of shortcutLines) {
			shortcutFooter.add(
				new TextRenderable(this.renderer, {
					content,
					height: 1,
					fg: this.sidebarTheme.getFgColor("dim"),
					truncate: true,
				}),
			);
		}
		this.root.add(shortcutFooter);
		this.rebuildList();
	}

	private rebuildList(): void {
		const list = this.list;
		if (list.isDestroyed) return;
		const sessions = this.getDisplayedSessions();
		if (this.updateStableRows(sessions)) return;
		clearChildren(list);
		this.marqueeEntries.clear();
		this.rowRecords.clear();
		this.renderedPaths = sessions.map((session) => session.path);
		if (sessions.length === 0) {
			list.add(
				new TextRenderable(this.renderer, {
					content: this.searchActive ? "No matching conversations" : "No conversations yet",
					fg: this.sidebarTheme.getFgColor("muted"),
					wrapMode: "word",
				}),
			);
			return;
		}

		for (let index = 0; index < sessions.length; index++) {
			const session = sessions[index]!;
			const selected = this.focused && index === this.selectedIndex;
			const confirming = this.itemState.kind === "confirm-delete" && this.itemState.path === session.path;
			const status =
				this.itemState.kind === "status" && this.itemState.path === session.path ? this.itemState : undefined;
			const foreground = session.state === "foreground";
			const selectedText = selected ? OPEN_TUI_COLORS.selectionText : undefined;
			const activateFromMouse = (event: MouseEvent) => {
				this.selectedIndex = index;
				this.selectedPath = session.path;
				event.preventDefault();
				event.stopPropagation();
				if (this.searchActive) this.stopSearch();
				this.activateSession(session.path);
			};
			const row = new BoxRenderable(this.renderer, {
				width: "100%",
				height: 3,
				flexDirection: "column",
				backgroundColor: foreground
					? OPEN_TUI_COLORS.primaryStrong
					: selected
						? OPEN_TUI_COLORS.selection
						: undefined,
				onMouseDown: activateFromMouse,
			});
			const foregroundText = foreground ? OPEN_TUI_COLORS.primaryText : selectedText;
			const titleRow = new BoxRenderable(this.renderer, {
				width: "100%",
				height: 1,
				flexDirection: "row",
				gap: 1,
				paddingLeft: 1,
				onMouseDown: activateFromMouse,
			});
			const stateNode = new TextRenderable(this.renderer, {
				content: STATE_ICON[session.state],
				fg: foregroundText ?? this.sidebarTheme.getFgColor(confirming ? "error" : stateColor(session.state)),
				flexShrink: 0,
				onMouseDown: activateFromMouse,
			});
			titleRow.add(stateNode);
			const title = normalizePreview(session.name ?? session.firstMessage) || "(empty conversation)";
			const titleNode = new TextRenderable(this.renderer, {
				content: title,
				fg:
					foregroundText ??
					this.sidebarTheme.getFgColor(confirming ? "error" : status?.tone === "error" ? "error" : "text"),
				attributes: foreground || selected ? TextAttributes.BOLD : TextAttributes.NONE,
				truncate: true,
				flexGrow: 1,
				minWidth: 0,
				onMouseDown: activateFromMouse,
			});
			titleRow.add(titleNode);
			row.add(titleRow);

			const previewRow = new BoxRenderable(this.renderer, {
				width: "100%",
				height: 1,
				flexDirection: "row",
				paddingLeft: 1,
				paddingRight: 1,
				onMouseDown: activateFromMouse,
			});
			let previewNode: TextRenderable;
			if (confirming) {
				previewNode = new TextRenderable(this.renderer, {
					content: "Press d again to delete",
					fg: foregroundText ?? this.sidebarTheme.getFgColor("error"),
					truncate: true,
					flexGrow: 1,
					minWidth: 0,
					onMouseDown: activateFromMouse,
				});
				previewRow.add(previewNode);
			} else if (status) {
				previewNode = new TextRenderable(this.renderer, {
					content: status.message,
					fg: foregroundText ?? this.sidebarTheme.getFgColor(status.tone === "error" ? "error" : "accent"),
					truncate: true,
					flexGrow: 1,
					minWidth: 0,
					onMouseDown: activateFromMouse,
				});
				previewRow.add(previewNode);
			} else {
				const result = this.searchResults?.find((candidate) => candidate.sessionPath === session.path);
				const titleEvidence = result?.evidence.kind === "title";
				const preview = normalizePreview(
					titleEvidence
						? (session.livePreview ?? session.lastMessage ?? "")
						: (result?.evidence.snippet ?? session.livePreview ?? session.lastMessage ?? ""),
				);
				const previewText = preview || (titleEvidence ? "Title match" : "No messages yet");
				previewNode = new TextRenderable(this.renderer, {
					content: previewText,
					fg: foregroundText ?? this.sidebarTheme.getFgColor(preview ? "muted" : "dim"),
					truncate: true,
					flexGrow: 1,
					minWidth: 0,
					onMouseDown: activateFromMouse,
				});
				previewRow.add(previewNode);
				if (session.state === "background-running") {
					this.marqueeEntries.set(session.path, {
						node: previewNode,
						text: previewText,
					});
				}
			}
			let throughputNode: TextRenderable | undefined;
			if (!confirming && !status && session.throughputTokensPerSecond !== undefined) {
				throughputNode = new TextRenderable(this.renderer, {
					content: formatThroughput(session.throughputTokensPerSecond).padStart(11),
					width: 11,
					fg: foregroundText ?? this.sidebarTheme.getFgColor("dim"),
					truncate: true,
					flexShrink: 0,
					onMouseDown: activateFromMouse,
				});
				previewRow.add(throughputNode);
			}
			row.add(previewRow);
			const metadataNode = new TextRenderable(this.renderer, {
				content: `${session.messageCount} msg${session.messageCount === 1 ? "" : "s"} · ${formatConversationActivityTime(session.modified)} · ${formatConversationCreatedTime(session.created)}`,
				height: 1,
				paddingLeft: 1,
				paddingRight: 1,
				fg: foregroundText ?? this.sidebarTheme.getFgColor("dim"),
				truncate: true,
				onMouseDown: activateFromMouse,
			});
			row.add(metadataNode);
			this.rowRecords.set(session.path, {
				row,
				stateNode,
				titleNode,
				previewNode,
				throughputNode,
				metadataNode,
			});
			list.add(row);
			if (index < sessions.length - 1) {
				list.add(
					new TextRenderable(this.renderer, {
						content: "─".repeat(OPEN_TUI_LAYOUT.sidebarMaxWidth),
						height: 1,
						fg: OPEN_TUI_COLORS.borderSubtle,
						truncate: true,
					}),
				);
			}
		}
	}

	private updateStableRows(sessions: readonly InteractiveSessionSummary[]): boolean {
		if (this.searchActive || this.itemState.kind !== "normal") return false;
		if (sessions.length === 0 || sessions.length !== this.renderedPaths.length) return false;
		for (let index = 0; index < sessions.length; index++) {
			const session = sessions[index]!;
			if (this.renderedPaths[index] !== session.path) return false;
			const record = this.rowRecords.get(session.path);
			if (!record) return false;
			if (Boolean(record.throughputNode) !== (session.throughputTokensPerSecond !== undefined)) return false;
		}

		this.marqueeEntries.clear();
		for (let index = 0; index < sessions.length; index++) {
			const session = sessions[index]!;
			const record = this.rowRecords.get(session.path)!;
			const selected = this.focused && index === this.selectedIndex;
			const foreground = session.state === "foreground";
			const foregroundText = foreground
				? OPEN_TUI_COLORS.primaryText
				: selected
					? OPEN_TUI_COLORS.selectionText
					: undefined;
			record.row.backgroundColor = foreground
				? OPEN_TUI_COLORS.primaryStrong
				: selected
					? OPEN_TUI_COLORS.selection
					: undefined;
			record.stateNode.content = STATE_ICON[session.state];
			record.stateNode.fg = foregroundText ?? this.sidebarTheme.getFgColor(stateColor(session.state));
			record.titleNode.content = normalizePreview(session.name ?? session.firstMessage) || "(empty conversation)";
			record.titleNode.fg = foregroundText ?? this.sidebarTheme.getFgColor("text");
			record.titleNode.attributes = foreground || selected ? TextAttributes.BOLD : TextAttributes.NONE;

			const preview = normalizePreview(session.livePreview ?? session.lastMessage ?? "");
			const previewText = preview || "No messages yet";
			record.previewNode.content = previewText;
			record.previewNode.fg = foregroundText ?? this.sidebarTheme.getFgColor(preview ? "muted" : "dim");
			if (session.state === "background-running") {
				this.marqueeEntries.set(session.path, { node: record.previewNode, text: previewText });
			}
			if (record.throughputNode) {
				record.throughputNode.content = formatThroughput(session.throughputTokensPerSecond).padStart(11);
				record.throughputNode.fg = foregroundText ?? this.sidebarTheme.getFgColor("dim");
			}
			record.metadataNode.content = `${session.messageCount} msg${session.messageCount === 1 ? "" : "s"} · ${formatConversationActivityTime(session.modified)} · ${formatConversationCreatedTime(session.created)}`;
			record.metadataNode.fg = foregroundText ?? this.sidebarTheme.getFgColor("dim");
		}
		return true;
	}

	private startMarquee(): void {
		if (this.marqueeTimer) return;
		this.marqueeTimer = setInterval(() => {
			if (this.root.isDestroyed) {
				if (this.marqueeTimer) clearInterval(this.marqueeTimer);
				this.marqueeTimer = undefined;
				return;
			}
			const list = this.list;
			if (!list.visible || !this.root.visible) return;
			const viewportTop = list.screenY;
			const viewportBottom = viewportTop + list.viewport.height;
			for (const entry of this.marqueeEntries.values()) {
				if (!entry.node.visible || entry.node.width <= 0) continue;
				const previewTop = entry.node.screenY;
				const previewBottom = previewTop + entry.node.height;
				if (previewBottom <= viewportTop || previewTop >= viewportBottom) continue;
				const latest = latestPreviewWindow(entry.text, entry.node.width);
				if (entry.node.plainText !== latest) entry.node.content = latest;
			}
		}, OPEN_TUI_LAYOUT.marqueeIntervalMs);
		this.marqueeTimer.unref?.();
	}

	private moveSearchSelection(delta: number): void {
		const previousPath = this.selectedPath;
		this.moveSelection(delta);
		if (!this.selectedPath || this.selectedPath === previousPath) return;
		this.previewedSearchPath = this.selectedPath;
		this.onPreviewSession?.(this.selectedPath);
	}

	private moveSelection(delta: number): void {
		const sessions = this.getDisplayedSessions();
		if (sessions.length === 0) return;
		this.selectedIndex = Math.max(0, Math.min(sessions.length - 1, this.selectedIndex + delta));
		this.selectedPath = sessions[this.selectedIndex]?.path;
		if (!this.searchActive && delta > 0 && this.selectedIndex >= sessions.length - 5) this.onLoadMore?.();
		this.rebuildList();
	}

	private getDisplayedSessions(): InteractiveSessionSummary[] {
		if (!this.searchActive) return this.sessions;
		if (!this.searchResults) {
			const query = this.searchQueryValue.trim().toLowerCase();
			if (!query) return this.sessions;
			return this.sessions.filter((session) =>
				[session.name, session.firstMessage, session.livePreview, session.lastMessage, session.path].some((value) =>
					value?.toLowerCase().includes(query),
				),
			);
		}
		const sessionsByPath = new Map(this.sessions.map((session) => [session.path, session]));
		return this.searchResults.flatMap((result) => {
			const session = sessionsByPath.get(result.sessionPath);
			return session ? [session] : [];
		});
	}

	private reconcileSelection(): void {
		const sessions = this.getDisplayedSessions();
		const selectedIndex = sessions.findIndex((session) => session.path === this.selectedPath);
		const foregroundIndex = sessions.findIndex((session) => session.state === "foreground");
		this.selectedIndex = selectedIndex >= 0 ? selectedIndex : Math.max(0, foregroundIndex);
		this.selectedPath = sessions[this.selectedIndex]?.path;
	}
}
