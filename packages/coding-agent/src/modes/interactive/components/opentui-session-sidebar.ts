import type {
	BoneContainerNode,
	BoneKeyEvent,
	BoneNode,
	BoneRenderContext,
	BoneScrollViewNode,
	BoneTextareaNode,
	BoneView,
} from "@frelion/bone-tui";
import type { InteractiveSessionSummary } from "../../../core/interactive-session-host.ts";
import type { MemorySearchResult } from "../../../core/memory.ts";
import { matchesOpenTUIAction } from "../opentui-keymap.ts";
import { type Theme, type ThemeColor, theme } from "../theme/theme.ts";
import { formatConversationActivityTime, formatConversationCreatedTime } from "./conversation-time.ts";

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
	private headerCount: ReturnType<BoneRenderContext["createText"]> | undefined;
	private searchInput: BoneTextareaNode | undefined;
	private searchStatusNode: ReturnType<BoneRenderContext["createText"]> | undefined;
	private list: BoneScrollViewNode | undefined;
	private focused = false;

	public onActivateSession?: (sessionPath: string) => void;
	public onPreviewSession?: (sessionPath: string) => void;
	public onDeleteSession?: (sessionPath: string, replacementPath: string | undefined) => void;
	public onSearchQueryChange?: (query: string) => void;
	public onSearchStateChange?: (active: boolean) => void;
	public onFocusChat?: () => void;
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
		});
		this.rebuildChrome();
		return this.root;
	}

	setFocused(focused: boolean): void {
		this.focused = focused;
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
		this.sessions = sessions;
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
			if (matchesOpenTUIAction(event, "confirm")) {
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
			if (selected) this.onActivateSession?.(selected.path);
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
				this.onActivateSession?.(selected.path);
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

	private rebuildChrome(): void {
		const context = this.context;
		const root = this.root;
		if (!context || !root) return;
		root.clear();
		const displayedSessions = this.getDisplayedSessions();
		const header = context.createBox({
			width: "100%",
			height: 1,
			flexDirection: "row",
			justifyContent: "space-between",
		});
		header.append(
			context.createText({
				content: this.searchActive ? "Search conversations" : "Conversations",
				fg: this.sidebarTheme.getFgColor("accent"),
				bold: true,
				truncate: true,
				flexShrink: 1,
			}),
		);
		this.headerCount = context.createText({
			content: String(displayedSessions.length),
			fg: this.sidebarTheme.getFgColor("dim"),
			flexShrink: 0,
		});
		header.append(this.headerCount);
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
		this.rebuildList();
	}

	private rebuildList(): void {
		const context = this.context;
		const list = this.list;
		if (!context || !list) return;
		list.clear();
		const sessions = this.getDisplayedSessions();
		if (this.headerCount) this.headerCount.content = String(sessions.length);
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
			const row = context.createBox({
				width: "100%",
				flexDirection: "column",
				paddingY: 1,
				backgroundColor: selected ? this.sidebarTheme.getBgColor("sidebarSelectedBg") : undefined,
				onMouseDown: (event) => {
					this.selectedIndex = index;
					this.selectedPath = session.path;
					this.rebuildList();
					event.preventDefault();
					event.stopPropagation();
				},
			});
			const titleRow = context.createBox({ width: "100%", flexDirection: "row", gap: 1 });
			titleRow.append(
				context.createText({
					content: STATE_ICON[session.state],
					fg: this.sidebarTheme.getFgColor(confirming ? "error" : stateColor(session.state)),
					flexShrink: 0,
				}),
			);
			const title = normalizePreview(session.name ?? session.firstMessage) || "(empty conversation)";
			titleRow.append(
				context.createText({
					content: confirming ? "Delete? Enter confirm · Esc cancel" : (status?.message ?? title),
					fg: this.sidebarTheme.getFgColor(
						confirming ? "error" : status?.tone === "error" ? "error" : status ? "accent" : "text",
					),
					bold: selected,
					truncate: true,
					flexGrow: 1,
					minWidth: 0,
				}),
			);
			const stateLabel =
				session.state === "background-running"
					? "run"
					: session.state === "background-waiting"
						? "wait"
						: undefined;
			const activity = formatConversationActivityTime(session.modified);
			titleRow.append(
				context.createText({
					content: stateLabel ? `${activity} · ${stateLabel}` : activity,
					fg: this.sidebarTheme.getFgColor("dim"),
					flexShrink: 0,
				}),
			);
			row.append(titleRow);

			if (confirming) {
				row.append(
					context.createText({ content: "Delete this conversation?", fg: this.sidebarTheme.getFgColor("error") }),
				);
				row.append(
					context.createText({ content: "Enter confirm · Esc cancel", fg: this.sidebarTheme.getFgColor("muted") }),
				);
			} else if (status) {
				row.append(
					context.createText({
						content: status.message,
						fg: this.sidebarTheme.getFgColor(status.tone === "error" ? "error" : "accent"),
						truncate: true,
					}),
				);
				row.append(this.createMetadata(context, session));
			} else {
				const result = this.searchResults?.find((candidate) => candidate.sessionPath === session.path);
				const titleEvidence = result?.evidence.kind === "title";
				const role =
					(titleEvidence ? undefined : result?.evidence.label) ??
					(session.lastMessageRole === "assistant"
						? "Bone"
						: session.lastMessageRole === "user"
							? "You"
							: "Message");
				const preview = normalizePreview(
					titleEvidence ? (session.lastMessage ?? "") : (result?.evidence.snippet ?? session.lastMessage ?? ""),
				);
				row.append(
					context.createText({
						content: preview ? `${role} · ${preview}` : titleEvidence ? "Title match" : "No messages yet",
						fg: this.sidebarTheme.getFgColor(preview ? "muted" : "dim"),
						truncate: true,
					}),
				);
				row.append(this.createMetadata(context, session));
			}
			list.append(row);
		}
	}

	private createMetadata(context: BoneRenderContext, session: InteractiveSessionSummary): BoneNode {
		return context.createText({
			content: `${formatConversationCreatedTime(session.created)} · ${session.messageCount} ${session.messageCount === 1 ? "msg" : "msgs"}`,
			fg: this.sidebarTheme.getFgColor("dim"),
			truncate: true,
		});
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
				[session.name, session.firstMessage, session.lastMessage, session.path].some((value) =>
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
