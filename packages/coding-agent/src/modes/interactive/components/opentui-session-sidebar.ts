import {
	type BoneContainerNode,
	type BoneKeyEvent,
	type BoneMouseEvent,
	type BoneNode,
	type BoneRenderContext,
	type BoneScrollViewNode,
	type BoneTextareaNode,
	type BoneTextNode,
	type BoneView,
	visibleWidth,
} from "@frelion/bone-tui";
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

/** Keep a running conversation's preview anchored to its newest model output. */
function latestPreviewWindow(text: string, width: number): string {
	if (width <= 0 || visibleWidth(text) <= width) return text;
	const graphemes = Array.from(graphemeSegmenter.segment(text), ({ segment }) => segment);
	const tail: string[] = [];
	let usedWidth = 0;
	for (let index = graphemes.length - 1; index >= 0; index--) {
		const grapheme = graphemes[index]!;
		const graphemeWidth = visibleWidth(grapheme);
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

function consume(event: BoneKeyEvent): true {
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
	node: BoneTextNode;
	text: string;
}

/** Structured OpenTUI conversation sidebar with search and session actions. */
export class OpenTUISessionSidebar implements BoneView {
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
	private context: BoneRenderContext | undefined;
	private root: BoneContainerNode | undefined;
	private searchInput: BoneTextareaNode | undefined;
	private searchStatusNode: ReturnType<BoneRenderContext["createText"]> | undefined;
	private list: BoneScrollViewNode | undefined;
	private focused = false;
	private frozenOrder: string[] | undefined;
	private readonly marqueeEntries = new Map<string, MarqueeEntry>();
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

	constructor(sidebarTheme: Theme = theme) {
		this.sidebarTheme = sidebarTheme;
	}

	get searchActive(): boolean {
		return this.searchMode;
	}

	get searchQuery(): string | undefined {
		return this.searchActive ? this.searchQueryValue : undefined;
	}

	mount(context: BoneRenderContext): BoneNode {
		if (this.root) throw new Error("OpenTUISessionSidebar is already mounted");
		this.context = context;
		this.root = context.createBox({
			width: "100%",
			height: "100%",
			flexDirection: "column",
			focusable: true,
			onMouseDown: () => this.onFocusRequest?.(),
		});
		this.rebuildChrome();
		this.startMarquee();
		return this.root;
	}

	setFocused(focused: boolean): void {
		if (focused === this.focused) {
			if (focused) (this.searchInput ?? this.root)?.focus();
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
		if (focused) {
			(this.searchInput ?? this.root)?.focus();
		} else {
			(this.searchInput ?? this.root)?.blur();
		}
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
		this.searchInput?.focus();
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
		if (this.focused) this.root?.focus();
		this.onSearchQueryChange?.("");
		this.onSearchStateChange?.(false);
		if (restorePath) this.onPreviewSession?.(restorePath);
	}

	handleKey(event: BoneKeyEvent): boolean {
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

	private handleSearchKey(event: BoneKeyEvent): boolean {
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
		const context = this.context;
		const root = this.root;
		if (!context || !root) return;
		root.clear();
		const header = context.createBox({
			width: "100%",
			height: 2,
			flexDirection: "column",
			alignItems: "center",
			justifyContent: "center",
		});
		header.append(
			context.createText({
				content: "CONVERSATIONS",
				fg: OPEN_TUI_COLORS.muted,
				bold: true,
				truncate: true,
				width: "100%",
			}),
		);
		root.append(header);

		if (this.searchStartPath !== undefined) {
			this.searchInput = context.createTextarea({
				width: "100%",
				height: 1,
				initialValue: this.searchQueryValue,
				placeholder: "Search conversations",
				textColor: this.sidebarTheme.getFgColor("text"),
				focusedTextColor: this.sidebarTheme.getFgColor("text"),
				placeholderColor: this.sidebarTheme.getFgColor("muted"),
				onChange: (value) => {
					this.searchQueryValue = value;
					this.searchResults = undefined;
					this.reconcileSelection();
					this.rebuildList();
					this.onSearchQueryChange?.(value);
				},
			});
			root.append(this.searchInput);
			this.searchStatusNode = context.createText({
				content: this.searchStatus ?? "",
				fg: this.sidebarTheme.getFgColor("muted"),
				wrapMode: "word",
			});
			this.searchStatusNode.visible = Boolean(this.searchStatus);
			root.append(this.searchStatusNode);
		} else {
			this.searchInput = undefined;
			this.searchStatusNode = undefined;
		}

		this.list = context.createScrollView({ width: "100%", flexGrow: 1, minHeight: 0, scrollY: true });
		root.append(this.list);
		const shortcutFooter = context.createBox({
			width: "100%",
			height: 3,
			flexDirection: "column",
			flexShrink: 0,
		});
		shortcutFooter.append(
			context.createText({
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
			shortcutFooter.append(
				context.createText({
					content,
					height: 1,
					fg: this.sidebarTheme.getFgColor("dim"),
					truncate: true,
				}),
			);
		}
		root.append(shortcutFooter);
		this.rebuildList();
	}

	private rebuildList(): void {
		const context = this.context;
		const list = this.list;
		if (!context || !list) return;
		list.clear();
		this.marqueeEntries.clear();
		const sessions = this.getDisplayedSessions();
		if (sessions.length === 0) {
			list.append(
				context.createText({
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
			const activateFromMouse = (event: BoneMouseEvent) => {
				this.selectedIndex = index;
				this.selectedPath = session.path;
				event.preventDefault();
				event.stopPropagation();
				if (this.searchActive) this.stopSearch();
				this.activateSession(session.path);
			};
			const row = context.createBox({
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
			const titleRow = context.createBox({
				width: "100%",
				height: 1,
				flexDirection: "row",
				gap: 1,
				paddingLeft: 1,
				onMouseDown: activateFromMouse,
			});
			titleRow.append(
				context.createText({
					content: STATE_ICON[session.state],
					fg: foregroundText ?? this.sidebarTheme.getFgColor(confirming ? "error" : stateColor(session.state)),
					flexShrink: 0,
					onMouseDown: activateFromMouse,
				}),
			);
			const title = normalizePreview(session.name ?? session.firstMessage) || "(empty conversation)";
			titleRow.append(
				context.createText({
					content: title,
					fg:
						foregroundText ??
						this.sidebarTheme.getFgColor(confirming ? "error" : status?.tone === "error" ? "error" : "text"),
					bold: foreground || selected,
					truncate: true,
					flexGrow: 1,
					minWidth: 0,
					onMouseDown: activateFromMouse,
				}),
			);
			row.append(titleRow);

			const previewRow = context.createBox({
				width: "100%",
				height: 1,
				flexDirection: "row",
				paddingLeft: 1,
				paddingRight: 1,
				onMouseDown: activateFromMouse,
			});
			if (confirming) {
				previewRow.append(
					context.createText({
						content: "Press d again to delete",
						fg: foregroundText ?? this.sidebarTheme.getFgColor("error"),
						truncate: true,
						flexGrow: 1,
						minWidth: 0,
						onMouseDown: activateFromMouse,
					}),
				);
			} else if (status) {
				previewRow.append(
					context.createText({
						content: status.message,
						fg: foregroundText ?? this.sidebarTheme.getFgColor(status.tone === "error" ? "error" : "accent"),
						truncate: true,
						flexGrow: 1,
						minWidth: 0,
						onMouseDown: activateFromMouse,
					}),
				);
			} else {
				const result = this.searchResults?.find((candidate) => candidate.sessionPath === session.path);
				const titleEvidence = result?.evidence.kind === "title";
				const preview = normalizePreview(
					titleEvidence
						? (session.livePreview ?? session.lastMessage ?? "")
						: (result?.evidence.snippet ?? session.livePreview ?? session.lastMessage ?? ""),
				);
				const previewText = preview || (titleEvidence ? "Title match" : "No messages yet");
				const previewNode = context.createText({
					content: previewText,
					fg: foregroundText ?? this.sidebarTheme.getFgColor(preview ? "muted" : "dim"),
					truncate: true,
					flexGrow: 1,
					minWidth: 0,
					onMouseDown: activateFromMouse,
				});
				previewRow.append(previewNode);
				if (session.state === "background-running") {
					this.marqueeEntries.set(session.path, {
						node: previewNode,
						text: previewText,
					});
				}
			}
			if (!confirming && !status && session.throughputTokensPerSecond !== undefined) {
				previewRow.append(
					context.createText({
						content: formatThroughput(session.throughputTokensPerSecond).padStart(11),
						width: 11,
						fg: foregroundText ?? this.sidebarTheme.getFgColor("dim"),
						truncate: true,
						flexShrink: 0,
						onMouseDown: activateFromMouse,
					}),
				);
			}
			row.append(previewRow);
			row.append(
				context.createText({
					content: `${session.messageCount} msg${session.messageCount === 1 ? "" : "s"} · ${formatConversationActivityTime(session.modified)} · ${formatConversationCreatedTime(session.created)}`,
					height: 1,
					paddingLeft: 1,
					paddingRight: 1,
					fg: foregroundText ?? this.sidebarTheme.getFgColor("dim"),
					truncate: true,
					onMouseDown: activateFromMouse,
				}),
			);
			list.append(row);
			if (index < sessions.length - 1) {
				list.append(
					context.createText({
						content: "─".repeat(OPEN_TUI_LAYOUT.sidebarMaxWidth),
						height: 1,
						fg: OPEN_TUI_COLORS.borderSubtle,
						truncate: true,
					}),
				);
			}
		}
	}

	private startMarquee(): void {
		if (this.marqueeTimer) return;
		this.marqueeTimer = setInterval(() => {
			if (this.root?.destroyed) {
				if (this.marqueeTimer) clearInterval(this.marqueeTimer);
				this.marqueeTimer = undefined;
				return;
			}
			const list = this.list;
			if (!list?.effectivelyVisible) return;
			const viewportTop = list.screenY;
			const viewportBottom = viewportTop + list.viewportHeight;
			for (const entry of this.marqueeEntries.values()) {
				if (!entry.node.effectivelyVisible || entry.node.width <= 0) continue;
				const previewTop = entry.node.screenY;
				const previewBottom = previewTop + entry.node.height;
				if (previewBottom <= viewportTop || previewTop >= viewportBottom) continue;
				const latest = latestPreviewWindow(entry.text, entry.node.width);
				if (entry.node.content !== latest) entry.node.content = latest;
			}
		}, OPEN_TUI_LAYOUT.marqueeIntervalMs);
		this.marqueeTimer.unref?.();
	}

	private moveSearchSelection(delta: number): void {
		const previousPath = this.selectedPath;
		this.moveSelection(delta);
		this.searchInput?.focus();
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
