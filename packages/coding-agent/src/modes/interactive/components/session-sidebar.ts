import {
	type Component,
	type Focusable,
	getKeybindings,
	Input,
	sliceByColumn,
	truncateToWidth,
	visibleWidth,
} from "@frelion/bone-tui";
import type { InteractiveSessionSummary } from "../../../core/interactive-session-host.ts";
import type { MemorySearchResult } from "../../../core/memory.ts";
import { theme } from "../theme/theme.ts";

const STATE_ICON: Record<InteractiveSessionSummary["state"], string> = {
	foreground: "●",
	"background-running": "↻",
	"background-waiting": "!",
	cold: "○",
};

const DETAIL_ROWS_PER_CONVERSATION = 3;
const DIVIDER_ROWS_PER_CONVERSATION = 1;
const MIN_ROWS_FOR_DETAILED_LIST = DETAIL_ROWS_PER_CONVERSATION * 2 + DIVIDER_ROWS_PER_CONVERSATION;

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

/**
 * Truncate unstyled content before applying the row's ANSI styling.
 *
 * `truncateToWidth` deliberately emits a full ANSI reset around its ellipsis
 * when its input already contains ANSI sequences. Applying it inside a styled
 * Side row would make the ellipsis fall back to the terminal's foreground and
 * background instead of inheriting the conversation row's state.
 */
function truncateSidebarText(text: string, width: number, ellipsis = "…"): string {
	if (width <= 0) return "";
	if (visibleWidth(text) <= width) return text;

	const ellipsisWidth = visibleWidth(ellipsis);
	if (ellipsisWidth >= width) return sliceByColumn(ellipsis, 0, width, true);
	return `${sliceByColumn(text, 0, width - ellipsisWidth, true)}${ellipsis}`;
}

function calendarDayDifference(earlier: Date, later: Date): number {
	const earlierDay = new Date(earlier.getFullYear(), earlier.getMonth(), earlier.getDate()).getTime();
	const laterDay = new Date(later.getFullYear(), later.getMonth(), later.getDate()).getTime();
	return Math.round((laterDay - earlierDay) / 86_400_000);
}

function formatTimeOfDay(date: Date): string {
	return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

function formatCalendarDate(date: Date, now: Date): string {
	const options: Intl.DateTimeFormatOptions =
		date.getFullYear() === now.getFullYear()
			? { month: "short", day: "numeric" }
			: { month: "short", day: "numeric", year: "numeric" };
	return new Intl.DateTimeFormat("en-US", options).format(date);
}

export function formatConversationActivityTime(date: Date, now = new Date()): string {
	if (Number.isNaN(date.getTime())) return "—";
	const elapsed = Math.max(0, now.getTime() - date.getTime());
	if (elapsed < 60_000) return "now";
	if (elapsed < 60 * 60_000) return `${Math.floor(elapsed / 60_000)}m`;

	const daysAgo = calendarDayDifference(date, now);
	if (daysAgo === 0) return formatTimeOfDay(date);
	if (daysAgo === 1) return "yesterday";
	if (daysAgo > 1 && daysAgo < 7) return new Intl.DateTimeFormat("en-US", { weekday: "short" }).format(date);
	return formatCalendarDate(date, now);
}

export function formatConversationCreatedTime(date: Date, now = new Date()): string {
	if (Number.isNaN(date.getTime())) return "created unknown";
	const daysAgo = calendarDayDifference(date, now);
	if (daysAgo === 0) return `created ${formatTimeOfDay(date)}`;
	if (daysAgo === 1) return "created yesterday";
	return `created ${formatCalendarDate(date, now)}`;
}

/** A focusable, viewport-bounded conversation list rendered beside the chat. */
export class SessionSidebar implements Component, Focusable {
	private sessions: InteractiveSessionSummary[] = [];
	private selectedIndex = 0;
	private selectedPath: string | undefined;
	private viewportRows = Number.POSITIVE_INFINITY;
	private itemState: SidebarItemState = { kind: "normal" };
	private searchInput: Input | undefined;
	private searchResults: readonly MemorySearchResult[] | undefined;
	private searchStatus: string | undefined;
	private lastSearchQuery = "";
	private searchStartPath: string | undefined;
	private previewedSearchPath: string | undefined;
	focused = false;
	public onActivateSession?: (sessionPath: string) => void;
	/** Switch the foreground conversation while Side search remains active. */
	public onPreviewSession?: (sessionPath: string) => void;
	public onDeleteSession?: (sessionPath: string, replacementPath: string | undefined) => void;
	/** Called as the user edits the Side search query. Search execution is provided by the host. */
	public onSearchQueryChange?: (query: string) => void;
	public onSearchStateChange?: (active: boolean) => void;
	public onFocusChat?: () => void;
	public onScrollChat?: (direction: "up" | "down") => void;
	public onLoadMore?: () => void;
	/** Keep application-level quit/interrupt behavior available while Side owns focus. */
	public onInterrupt?: () => void;
	public onExit?: () => void;

	get searchActive(): boolean {
		return this.searchInput !== undefined;
	}

	get searchQuery(): string | undefined {
		return this.searchInput?.getValue();
	}

	setSessions(sessions: InteractiveSessionSummary[]): void {
		this.sessions = sessions;
		this.reconcileSelection();
		const itemState = this.itemState;
		if (itemState.kind !== "normal" && !sessions.some((session) => session.path === itemState.path)) {
			this.itemState = { kind: "normal" };
		}
	}

	/** Replace the visible session order with ranked, explainable search results. */
	setSearchResults(results: readonly MemorySearchResult[] | undefined): void {
		if (!this.searchInput) return;
		this.searchResults = results;
		this.reconcileSelection();
	}

	/** Render local indexing and model-download progress independently from a session row. */
	setSearchStatus(status: string | undefined): void {
		this.searchStatus = status;
	}

	startSearch(): void {
		if (this.searchInput) return;
		// Capture the active selection before an existing query temporarily clears
		// the result list. Escape must always restore this exact conversation.
		this.searchStartPath = this.selectedPath;
		this.previewedSearchPath = undefined;
		this.itemState = { kind: "normal" };
		this.searchResults = this.lastSearchQuery.trim() ? [] : undefined;
		this.searchStatus = undefined;
		const input = new Input();
		input.setValue(this.lastSearchQuery, this.lastSearchQuery.length);
		input.focused = true;
		input.onEscape = () => this.stopSearch({ restorePreview: true });
		this.searchInput = input;
		this.reconcileSelection();
		this.onSearchQueryChange?.(this.lastSearchQuery);
		this.onSearchStateChange?.(true);
	}

	stopSearch(options?: { restorePreview?: boolean }): void {
		if (!this.searchInput) return;
		const restorePath =
			options?.restorePreview && this.previewedSearchPath && this.previewedSearchPath !== this.searchStartPath
				? this.searchStartPath
				: undefined;
		this.lastSearchQuery = this.searchInput.getValue();
		this.searchInput.focused = false;
		this.searchInput = undefined;
		this.searchResults = undefined;
		this.searchStatus = undefined;
		this.searchStartPath = undefined;
		this.previewedSearchPath = undefined;
		if (restorePath) this.selectedPath = restorePath;
		this.reconcileSelection();
		this.onSearchQueryChange?.("");
		this.onSearchStateChange?.(false);
		if (restorePath) this.onPreviewSession?.(restorePath);
	}

	setViewportRows(rows: number): void {
		this.viewportRows = Math.max(1, rows);
	}

	setStatusMessage(message: string | undefined, tone: "success" | "error" = "success"): void {
		const selected = this.getDisplayedSessions()[this.selectedIndex];
		this.itemState =
			message && selected ? { kind: "status", path: selected.path, tone, message } : { kind: "normal" };
	}

	invalidate(): void {}

	render(width: number): string[] {
		const viewportRows = Number.isFinite(this.viewportRows) ? Math.max(1, this.viewportRows) : undefined;
		const displayedSessions = this.getDisplayedSessions();
		const lines: string[] = [];
		const addSidebarLine = (line = "", selected = false) => {
			const truncated = truncateToWidth(line, width, "…");
			const padded = `${truncated}${" ".repeat(Math.max(0, width - visibleWidth(truncated)))}`;
			lines.push(selected ? theme.bg("sidebarSelectedBg", padded) : padded);
		};
		const addDivider = () => {
			addSidebarLine(theme.fg("borderMuted", "┄".repeat(Math.max(1, width))));
		};

		const header = this.searchInput ? "Search conversations" : "Conversations";
		const count = String(displayedSessions.length);
		const headerGap = " ".repeat(Math.max(1, width - visibleWidth(header) - visibleWidth(count) - 1));
		addSidebarLine(`${theme.bold(theme.fg("accent", header))}${headerGap}${theme.fg("dim", count)}`);
		if (this.searchInput) {
			const searchLine = this.searchInput.render(Math.max(1, width))[0] ?? "> ";
			addSidebarLine(theme.fg("text", searchLine));
			if (this.searchStatus) addSidebarLine(theme.fg("muted", this.searchStatus));
		}
		if (displayedSessions.length === 0) {
			addDivider();
			addSidebarLine(theme.fg("muted", this.searchInput ? "No matching conversations" : "No conversations yet"));
			return this.fillViewport(lines, width, viewportRows);
		}

		const remainingRows =
			viewportRows === undefined ? Number.POSITIVE_INFINITY : Math.max(1, viewportRows - lines.length);
		const detailedRows = remainingRows >= MIN_ROWS_FOR_DETAILED_LIST;
		const listRows = detailedRows ? Math.max(0, remainingRows - 1) : remainingRows;
		let visibleCount = this.getVisibleCount(listRows, detailedRows, false);
		let showOverflow = displayedSessions.length > visibleCount;
		if (showOverflow) {
			const countWithOverflow = this.getVisibleCount(listRows, detailedRows, true);
			if (countWithOverflow > 0) {
				visibleCount = countWithOverflow;
				showOverflow = displayedSessions.length > visibleCount;
			} else {
				showOverflow = false;
			}
		}
		const startIndex = Math.max(
			0,
			Math.min(this.selectedIndex - Math.floor(visibleCount / 2), displayedSessions.length - visibleCount),
		);
		const endIndex = Math.min(displayedSessions.length, startIndex + visibleCount);
		if (detailedRows) addDivider();

		for (let index = startIndex; index < endIndex; index++) {
			const session = displayedSessions[index]!;
			const searchResult = this.searchResults?.find((result) => result.sessionPath === session.path);
			const icon = STATE_ICON[session.state];
			const title = normalizePreview(session.name ?? session.firstMessage) || "(empty conversation)";
			const selected = this.focused && index === this.selectedIndex;
			const isConfirmingDelete = this.itemState.kind === "confirm-delete" && session.path === this.itemState.path;
			const status =
				this.itemState.kind === "status" && session.path === this.itemState.path ? this.itemState : undefined;
			const prefix = `${icon} `;
			const statusColor = isConfirmingDelete
				? "error"
				: session.state === "foreground"
					? "accent"
					: session.state === "background-running"
						? "warning"
						: session.state === "background-waiting"
							? "error"
							: "muted";
			const activity = formatConversationActivityTime(session.modified);
			const stateLabel =
				session.state === "background-running"
					? "run"
					: session.state === "background-waiting"
						? "wait"
						: undefined;
			const activityLabel = stateLabel ? `${activity} · ${stateLabel}` : activity;
			const titleWidth = Math.max(1, width - visibleWidth(prefix) - visibleWidth(activityLabel) - 1);
			const compactStateLabel = isConfirmingDelete ? "Delete? Enter confirm · Esc cancel" : status?.message;
			const titleLabel = truncateSidebarText(compactStateLabel ?? title, titleWidth);
			const titleGap = " ".repeat(
				Math.max(1, width - visibleWidth(prefix) - visibleWidth(titleLabel) - visibleWidth(activityLabel)),
			);
			const titleText = compactStateLabel
				? theme.fg(isConfirmingDelete ? "error" : status?.tone === "error" ? "error" : "accent", titleLabel)
				: theme.fg("text", titleLabel);
			const titleLine = `${theme.fg(statusColor, prefix)}${selected ? theme.bold(titleText) : titleText}${titleGap}${theme.fg("dim", activityLabel)}`;
			addSidebarLine(titleLine, selected);

			if (!detailedRows) continue;
			if (isConfirmingDelete) {
				addSidebarLine(theme.fg("error", "    Delete this conversation?"), selected);
				addSidebarLine(theme.fg("muted", "    Enter confirm · Esc cancel"), selected);
				if (index + 1 < endIndex) addDivider();
				continue;
			}
			if (status) {
				addSidebarLine(theme.fg(status.tone === "error" ? "error" : "accent", `    ${status.message}`), selected);
				const metadata = `${formatConversationCreatedTime(session.created)} · ${session.messageCount} ${session.messageCount === 1 ? "msg" : "msgs"}`;
				addSidebarLine(theme.fg("dim", `    ${truncateSidebarText(metadata, Math.max(1, width - 4))}`), selected);
				if (index + 1 < endIndex) addDivider();
				continue;
			}

			const hasTitleEvidence = searchResult?.evidence.kind === "title";
			const role =
				(hasTitleEvidence ? undefined : searchResult?.evidence.label) ??
				(session.lastMessageRole === "assistant" ? "Bone" : session.lastMessageRole === "user" ? "You" : undefined);
			const preview = normalizePreview(
				hasTitleEvidence
					? (session.lastMessage ?? "")
					: (searchResult?.evidence.snippet ?? session.lastMessage ?? ""),
			);
			const metadata = `${formatConversationCreatedTime(session.created)} · ${session.messageCount} ${session.messageCount === 1 ? "msg" : "msgs"}`;
			const metadataLine = `    ${truncateSidebarText(metadata, Math.max(1, width - 4))}`;
			const previewPrefix = `    ${role ?? "Message"} · `;
			const previewContentWidth = Math.max(1, width - visibleWidth(previewPrefix));
			const styledPreview = preview
				? `${theme.fg("dim", previewPrefix)}${theme.fg("muted", truncateSidebarText(preview, previewContentWidth))}`
				: theme.fg("dim", hasTitleEvidence ? "    Title match" : "    No messages yet");
			const styledMetadata = theme.fg("dim", metadataLine);
			addSidebarLine(styledPreview, selected);
			addSidebarLine(styledMetadata, selected);
			if (index + 1 < endIndex) addDivider();
		}

		if (showOverflow) {
			if (detailedRows) addDivider();
			addSidebarLine(theme.fg("muted", `… ${displayedSessions.length - visibleCount} more`));
		}

		return this.fillViewport(lines, width, viewportRows);
	}

	private getVisibleCount(listRows: number, detailed: boolean, reserveOverflow: boolean): number {
		if (!Number.isFinite(listRows)) return this.sessions.length;
		if (!detailed) return Math.max(1, Math.floor((listRows - (reserveOverflow ? 1 : 0)) / 1));
		const available = listRows - (reserveOverflow ? 1 : 0);
		return available < DETAIL_ROWS_PER_CONVERSATION
			? 0
			: Math.floor(
					(available + DIVIDER_ROWS_PER_CONVERSATION) /
						(DETAIL_ROWS_PER_CONVERSATION + DIVIDER_ROWS_PER_CONVERSATION),
				);
	}

	private fillViewport(lines: string[], width: number, viewportRows: number | undefined): string[] {
		if (viewportRows === undefined) return lines;
		while (lines.length < viewportRows) {
			lines.push(" ".repeat(width));
		}
		return lines.slice(0, viewportRows);
	}

	handleInput(data: string): void {
		const keybindings = getKeybindings();
		if (this.itemState.kind === "confirm-delete") {
			if (keybindings.matches(data, "tui.select.confirm")) {
				const sessionPath = this.itemState.path;
				this.itemState = { kind: "normal" };
				const selectedIndex = this.sessions.findIndex((session) => session.path === sessionPath);
				const replacementPath =
					this.sessions[selectedIndex + 1]?.path ?? this.sessions[Math.max(0, selectedIndex - 1)]?.path;
				this.onDeleteSession?.(sessionPath, replacementPath === sessionPath ? undefined : replacementPath);
				return;
			}
			if (keybindings.matches(data, "tui.select.cancel")) {
				this.itemState = { kind: "normal" };
				return;
			}
			return;
		}
		if (this.itemState.kind === "status") this.itemState = { kind: "normal" };
		if (keybindings.matches(data, "app.clear")) {
			this.onInterrupt?.();
			return;
		}
		if (keybindings.matches(data, "app.exit")) {
			this.onExit?.();
			return;
		}
		if (this.searchInput) {
			this.handleSearchInput(data, keybindings);
			return;
		}
		if (keybindings.matches(data, "app.focus.right")) {
			this.onFocusChat?.();
			return;
		}
		if (keybindings.matches(data, "tui.select.confirm")) {
			const selected = this.getDisplayedSessions()[this.selectedIndex];
			if (selected) {
				this.stopSearch();
				this.onActivateSession?.(selected.path);
			}
			return;
		}
		if (keybindings.matches(data, "tui.select.up")) {
			this.moveSelection(-1);
			return;
		}
		if (keybindings.matches(data, "tui.select.down")) {
			this.moveSelection(1);
			return;
		}
		if (keybindings.matches(data, "tui.select.pageUp")) {
			this.onScrollChat?.("up");
			return;
		}
		if (keybindings.matches(data, "tui.select.pageDown")) {
			this.onScrollChat?.("down");
			return;
		}
		if (keybindings.matches(data, "app.session.searchFromSidebar")) {
			this.startSearch();
			return;
		}
		if (keybindings.matches(data, "app.session.deleteFromSidebar")) {
			const selected = this.getDisplayedSessions()[this.selectedIndex];
			if (selected) this.itemState = { kind: "confirm-delete", path: selected.path };
		}
	}

	private handleSearchInput(data: string, keybindings: ReturnType<typeof getKeybindings>): void {
		if (keybindings.matches(data, "app.focus.right")) {
			this.stopSearch();
			this.onFocusChat?.();
			return;
		}
		if (keybindings.matches(data, "tui.select.confirm")) {
			const selected = this.getDisplayedSessions()[this.selectedIndex];
			if (selected) {
				this.stopSearch();
				this.onActivateSession?.(selected.path);
			}
			return;
		}
		if (keybindings.matches(data, "tui.select.up")) {
			this.moveSearchSelection(-1);
			return;
		}
		if (keybindings.matches(data, "tui.select.down")) {
			this.moveSearchSelection(1);
			return;
		}
		if (keybindings.matches(data, "tui.select.pageUp")) {
			this.onScrollChat?.("up");
			return;
		}
		if (keybindings.matches(data, "tui.select.pageDown")) {
			this.onScrollChat?.("down");
			return;
		}

		const input = this.searchInput;
		if (!input) return;
		const previousQuery = input.getValue();
		input.handleInput(data);
		if (this.searchInput !== input) return;
		const query = input.getValue();
		if (query !== previousQuery) {
			this.searchResults = query.trim() ? [] : undefined;
			this.reconcileSelection();
			this.onSearchQueryChange?.(query);
		}
	}

	private moveSearchSelection(delta: number): void {
		const previousPath = this.selectedPath;
		this.moveSelection(delta);
		if (!this.selectedPath || this.selectedPath === previousPath) return;
		this.previewedSearchPath = this.selectedPath;
		this.onPreviewSession?.(this.selectedPath);
	}

	private moveSelection(delta: number): void {
		const displayedSessions = this.getDisplayedSessions();
		if (displayedSessions.length === 0) return;
		this.selectedIndex = Math.max(0, Math.min(displayedSessions.length - 1, this.selectedIndex + delta));
		this.selectedPath = displayedSessions[this.selectedIndex]?.path;
		if (!this.searchInput && delta > 0 && this.selectedIndex >= displayedSessions.length - 5) {
			this.onLoadMore?.();
		}
	}

	private getDisplayedSessions(): InteractiveSessionSummary[] {
		if (!this.searchInput || !this.searchResults) return this.sessions;
		const sessionsByPath = new Map(this.sessions.map((session) => [session.path, session]));
		return this.searchResults.flatMap((result) => {
			const session = sessionsByPath.get(result.sessionPath);
			return session ? [session] : [];
		});
	}

	private reconcileSelection(): void {
		const displayedSessions = this.getDisplayedSessions();
		const selectedIndex = displayedSessions.findIndex((session) => session.path === this.selectedPath);
		const foregroundIndex = displayedSessions.findIndex((session) => session.state === "foreground");
		this.selectedIndex = selectedIndex >= 0 ? selectedIndex : Math.max(0, foregroundIndex);
		this.selectedPath = displayedSessions[this.selectedIndex]?.path;
	}
}
