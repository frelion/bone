import { stripVTControlCharacters } from "node:util";
import { setKeybindings } from "@frelion/bone-tui";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import type { InteractiveSessionSummary } from "../src/core/interactive-session-host.ts";
import { KeybindingsManager } from "../src/core/keybindings.ts";
import { ChatScrollLayout } from "../src/modes/interactive/components/chat-scroll-layout.ts";
import {
	formatConversationActivityTime,
	formatConversationCreatedTime,
	SessionSidebar,
} from "../src/modes/interactive/components/session-sidebar.ts";
import { SplitPane } from "../src/modes/interactive/components/split-pane.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

function makeSession(id: string, state: InteractiveSessionSummary["state"]): InteractiveSessionSummary {
	return {
		path: `/sessions/${id}.jsonl`,
		id,
		cwd: "/workspace",
		created: new Date(),
		modified: new Date(),
		messageCount: 1,
		firstMessage: `Session ${id}`,
		allMessagesText: `Session ${id}`,
		lastMessage: `Latest message for ${id}`,
		lastMessageRole: "assistant",
		state,
	};
}

class StaticComponent {
	private readonly lines: string[];

	constructor(lines: string[]) {
		this.lines = lines;
	}

	invalidate(): void {}

	render(): string[] {
		return this.lines;
	}
}

beforeAll(() => {
	initTheme("dark");
});

beforeEach(() => {
	setKeybindings(new KeybindingsManager());
});

describe("SessionSidebar", () => {
	it("navigates sessions and opens the selected session", () => {
		const sidebar = new SessionSidebar();
		const activate = vi.fn();
		sidebar.onActivateSession = activate;
		sidebar.focused = true;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "cold")]);

		sidebar.handleInput("\x1b[B");
		sidebar.handleInput("\r");

		expect(activate).toHaveBeenCalledWith("/sessions/b.jsonl");
		expect(stripVTControlCharacters(sidebar.render(30).join("\n"))).toContain("○ Session b");
	});

	it("keeps Ctrl+C and Ctrl+D connected to application-level handlers while Side has focus", () => {
		const sidebar = new SessionSidebar();
		const interrupt = vi.fn();
		const exit = vi.fn();
		sidebar.onInterrupt = interrupt;
		sidebar.onExit = exit;

		sidebar.handleInput("\x03");
		sidebar.handleInput("\x04");

		expect(interrupt).toHaveBeenCalledTimes(1);
		expect(exit).toHaveBeenCalledTimes(1);
	});

	it("confirms Side deletion with d and preserves selection while confirming", () => {
		const sidebar = new SessionSidebar();
		const remove = vi.fn();
		sidebar.onDeleteSession = remove;
		sidebar.focused = true;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "cold")]);

		sidebar.handleInput("d");
		sidebar.handleInput("\x1b[B");
		const output = stripVTControlCharacters(sidebar.render(42).join("\n"));
		expect(output).toContain("Delete this conversation?");
		expect(output).toContain("Enter confirm · Esc cancel");

		sidebar.handleInput("\r");
		expect(remove).toHaveBeenCalledWith("/sessions/a.jsonl", "/sessions/b.jsonl");
	});

	it("cancels Side deletion confirmation with Escape", () => {
		const sidebar = new SessionSidebar();
		const remove = vi.fn();
		sidebar.onDeleteSession = remove;
		sidebar.focused = true;
		sidebar.setSessions([makeSession("a", "foreground")]);

		sidebar.handleInput("d");
		sidebar.handleInput("\x1b");
		sidebar.handleInput("\r");

		expect(remove).not.toHaveBeenCalled();
	});

	it("keeps the title compact and renders confirmation inside the selected conversation", () => {
		const sidebar = new SessionSidebar();
		sidebar.focused = true;
		sidebar.setSessions([makeSession("a", "foreground")]);

		const normalLines = stripVTControlCharacters(sidebar.render(42).join("\n")).split("\n");
		expect(normalLines[0]?.trimEnd()).toMatch(/^Conversations\s+1$/);
		expect(normalLines.join("\n")).not.toContain("Shift+→");
		expect(normalLines.join("\n")).not.toContain("d delete");

		sidebar.handleInput("d");
		const confirmationLines = stripVTControlCharacters(sidebar.render(42).join("\n")).split("\n");
		expect(confirmationLines[0]?.trimEnd()).toMatch(/^Conversations\s+1$/);
		expect(confirmationLines.join("\n")).toContain("Delete this conversation?");
	});

	it("aligns first-level sidebar content with the terminal left edge", () => {
		const sidebar = new SessionSidebar();
		sidebar.focused = true;
		sidebar.setSessions([makeSession("a", "foreground")]);

		const normalLines = stripVTControlCharacters(sidebar.render(42).join("\n")).split("\n");
		expect(normalLines[0]).toMatch(/^Conversations/);
		expect(normalLines.find((line) => line.includes("Session a"))).toMatch(/^● Session a/);
		expect(normalLines.join("\n")).not.toContain("›");

		sidebar.handleInput("/");
		sidebar.setSearchStatus("Searching");
		const searchLines = stripVTControlCharacters(sidebar.render(42).join("\n")).split("\n");
		expect(searchLines[0]).toMatch(/^Search conversations/);
		expect(searchLines[1]).toMatch(/^> /);
		expect(searchLines[2]).toBe("Searching".padEnd(42));

		const emptySidebar = new SessionSidebar();
		const emptyLines = stripVTControlCharacters(emptySidebar.render(42).join("\n")).split("\n");
		expect(emptyLines[2]).toMatch(/^No conversations yet/);
	});

	it("renders deletion feedback within the selected conversation", () => {
		const sidebar = new SessionSidebar();
		sidebar.focused = true;
		sidebar.setSessions([makeSession("a", "foreground")]);
		sidebar.setStatusMessage("Conversation moved to Bone Trash");

		const output = stripVTControlCharacters(sidebar.render(48).join("\n"));
		expect(output).toContain("Conversation moved to Bone Trash");
		expect(output).not.toContain("Shift+← focus Side");
	});

	it("keeps the sidebar within the terminal viewport", () => {
		const sidebar = new SessionSidebar();
		sidebar.setViewportRows(5);
		sidebar.setSessions([
			makeSession("a", "foreground"),
			makeSession("b", "cold"),
			makeSession("c", "cold"),
			makeSession("d", "cold"),
			makeSession("e", "cold"),
		]);

		expect(sidebar.render(30)).toHaveLength(5);
		expect(stripVTControlCharacters(sidebar.render(30).join("\n"))).toContain("more");
	});

	it("requests another conversation page near the loaded tail", () => {
		const sidebar = new SessionSidebar();
		const loadMore = vi.fn();
		sidebar.onLoadMore = loadMore;
		sidebar.setSessions(Array.from({ length: 10 }, (_, index) => makeSession(String(index), "cold")));
		for (let index = 0; index < 6; index++) sidebar.handleInput("\x1b[B");
		expect(loadMore).toHaveBeenCalled();
	});

	it("fills the complete viewport and separates detailed conversations", () => {
		const sidebar = new SessionSidebar();
		sidebar.setViewportRows(14);
		sidebar.focused = true;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "cold")]);

		const lines = sidebar.render(32);
		const output = stripVTControlCharacters(lines.join("\n"));
		expect(lines).toHaveLength(14);
		expect(lines.filter((line) => line.includes("\x1b[48;"))).toHaveLength(3);
		expect(output).toContain("┄");
		expect(output).toContain("Conversations");
	});

	it("renders conversation title, activity, preview, and creation metadata", () => {
		const sidebar = new SessionSidebar();
		const modified = new Date();
		const created = new Date(modified);
		created.setHours(9, 41, 0, 0);
		sidebar.setSessions([
			{
				...makeSession("details", "background-running"),
				name: "Refine Side information",
				created,
				modified,
				messageCount: 8,
				lastMessage: "I will add created and recent-message details.",
				lastMessageRole: "assistant",
			},
		]);

		const output = stripVTControlCharacters(sidebar.render(44).join("\n"));
		expect(output).toContain("Refine Side information");
		expect(output).toContain("now · run");
		expect(output).toContain("Bone · I will add created");
		expect(output).toContain("created 09:41 · 8 msgs");
	});

	it("keeps truncated selected-row text within its selected colors", () => {
		const sidebar = new SessionSidebar();
		sidebar.setViewportRows(8);
		sidebar.focused = true;
		sidebar.setSessions([
			{
				...makeSession("truncated", "foreground"),
				name: "A deliberately long conversation title that must be truncated",
				lastMessage: "A deliberately long preview that must be truncated too.",
			},
		]);

		const selectedLines = sidebar.render(32).filter((line) => line.includes("\x1b[48;5;238m"));
		expect(selectedLines).toHaveLength(3);
		for (const line of selectedLines) {
			expect(line).not.toContain("\x1b[0m…");
			expect(line).toMatch(/\x1b\[49m$/);
		}
		expect(stripVTControlCharacters(selectedLines.join("\n"))).toContain("…");
	});

	it("formats activity and creation time for the sidebar scan path", () => {
		const now = new Date(2026, 6, 17, 12, 0);
		expect(formatConversationActivityTime(new Date(2026, 6, 17, 11, 58), now)).toBe("2m");
		expect(formatConversationActivityTime(new Date(2026, 6, 16, 12, 0), now)).toBe("yesterday");
		expect(formatConversationCreatedTime(new Date(2026, 6, 17, 9, 41), now)).toBe("created 09:41");
		expect(formatConversationCreatedTime(new Date(2026, 6, 14, 9, 41), now)).toBe("created Jul 14");
	});

	it("keeps Side search live, exits on Enter, and restores the previous query", () => {
		const sidebar = new SessionSidebar();
		const queryChanges = vi.fn();
		const activateSession = vi.fn();
		const previewSession = vi.fn();
		const stateChanges = vi.fn();
		sidebar.onSearchQueryChange = queryChanges;
		sidebar.onActivateSession = activateSession;
		sidebar.onPreviewSession = previewSession;
		sidebar.onSearchStateChange = stateChanges;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "cold")]);

		sidebar.handleInput("/");
		expect(sidebar.searchActive).toBe(true);
		expect(stateChanges).toHaveBeenLastCalledWith(true);
		expect(stripVTControlCharacters(sidebar.render(42).join("\n"))).toContain("Search conversations");

		sidebar.handleInput("semantic cache");
		expect(sidebar.searchQuery).toBe("semantic cache");
		expect(queryChanges).toHaveBeenLastCalledWith("semantic cache");

		sidebar.setSearchResults([
			{
				sessionPath: "/sessions/b.jsonl",
				score: 1,
				evidence: { kind: "user", label: "You", snippet: "semantic cache implementation" },
			},
		]);
		const resultOutput = stripVTControlCharacters(sidebar.render(42).join("\n"));
		expect(resultOutput).toContain("Session b");
		expect(resultOutput).not.toContain("Session a");

		expect(resultOutput).toContain("semantic cache implementation");
		sidebar.handleInput("\r");
		expect(activateSession).toHaveBeenCalledWith("/sessions/b.jsonl");
		expect(previewSession).not.toHaveBeenCalled();
		expect(sidebar.searchActive).toBe(false);
		expect(stateChanges).toHaveBeenLastCalledWith(false);

		sidebar.handleInput("/");
		expect(sidebar.searchQuery).toBe("semantic cache");
		sidebar.handleInput(" implementation");
		expect(sidebar.searchQuery).toBe("semantic cache implementation");
		sidebar.handleInput("\x1b");
		expect(sidebar.searchActive).toBe(false);
		expect(stateChanges).toHaveBeenLastCalledWith(false);
	});

	it("previews arrow-selected search results and restores the original conversation on Escape", () => {
		const sidebar = new SessionSidebar();
		const previewSession = vi.fn();
		sidebar.focused = true;
		sidebar.onPreviewSession = previewSession;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "cold"), makeSession("c", "cold")]);

		sidebar.handleInput("/");
		sidebar.handleInput("semantic");
		sidebar.setSearchResults([
			{
				sessionPath: "/sessions/b.jsonl",
				score: 2,
				evidence: { kind: "user", label: "You", snippet: "semantic cache" },
			},
			{
				sessionPath: "/sessions/c.jsonl",
				score: 1,
				evidence: { kind: "assistant", label: "Bone", snippet: "semantic search" },
			},
		]);

		sidebar.handleInput("\x1b[B");
		expect(previewSession).toHaveBeenLastCalledWith("/sessions/c.jsonl");
		sidebar.handleInput("\x1b");
		expect(previewSession).toHaveBeenLastCalledWith("/sessions/a.jsonl");
		expect(sidebar.searchActive).toBe(false);
		expect(stripVTControlCharacters(sidebar.render(42).join("\n"))).toContain("● Session a");
	});

	it("restores the original conversation when reopening a saved query is cancelled", () => {
		const sidebar = new SessionSidebar();
		const previewSession = vi.fn();
		sidebar.onPreviewSession = previewSession;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "cold"), makeSession("c", "cold")]);

		sidebar.handleInput("/");
		sidebar.handleInput("semantic");
		sidebar.handleInput("\x1b");
		sidebar.handleInput("/");
		sidebar.setSearchResults([
			{
				sessionPath: "/sessions/b.jsonl",
				score: 2,
				evidence: { kind: "user", label: "You", snippet: "semantic cache" },
			},
			{
				sessionPath: "/sessions/c.jsonl",
				score: 1,
				evidence: { kind: "assistant", label: "Bone", snippet: "semantic search" },
			},
		]);

		sidebar.handleInput("\x1b[B");
		expect(previewSession).toHaveBeenLastCalledWith("/sessions/c.jsonl");
		sidebar.handleInput("\x1b");
		expect(previewSession).toHaveBeenLastCalledWith("/sessions/a.jsonl");
	});

	it("does not repeat a matching title as the result preview", () => {
		const sidebar = new SessionSidebar();
		sidebar.focused = true;
		sidebar.setSessions([makeSession("a", "foreground")]);
		sidebar.handleInput("/");
		sidebar.handleInput("session a");
		sidebar.setSearchResults([
			{
				sessionPath: "/sessions/a.jsonl",
				score: 1,
				evidence: { kind: "title", label: "Title", snippet: "Session a" },
			},
		]);

		const output = stripVTControlCharacters(sidebar.render(42).join("\n"));
		expect(output).toContain("Session a");
		expect(output).not.toContain("Title · Session a");
	});
});

describe("SplitPane", () => {
	it("shows a 40-column sidebar at its 86-column layout threshold", () => {
		const pane = new SplitPane(new StaticComponent(["Side"]), new StaticComponent(["Main"]), 40, "│ ", 44);

		expect(pane.render(85).map(stripVTControlCharacters)).toEqual(["Main"]);
		expect(pane.render(86).map(stripVTControlCharacters)).toEqual([`${"Side".padEnd(40)}│ Main`]);
	});

	it("anchors the sidebar to the active terminal viewport", () => {
		const pane = new SplitPane(
			new StaticComponent(["Sessions", "Session A"]),
			new StaticComponent(["0", "1", "2", "3", "4", "5"]),
			10,
			" | ",
			20,
			() => 3,
		);

		const lines = pane.render(40).map(stripVTControlCharacters);
		expect(lines).toHaveLength(6);
		expect(lines[3]).toContain("Sessions");
		expect(lines[4]).toContain("Session A");
	});

	it("keeps the sidebar in view while chat history is paged", () => {
		const layout = new ChatScrollLayout(
			new StaticComponent(["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]),
			new StaticComponent(["Composer"]),
			() => 5,
		);
		const pane = new SplitPane(new StaticComponent(["Sessions", "Session A"]), layout, 10, " | ", 20, () => 5);

		expect(pane.render(40).map(stripVTControlCharacters)).toEqual([
			expect.stringContaining("Sessions"),
			expect.stringContaining("Session A"),
			expect.stringContaining("8"),
			expect.stringContaining("9"),
			expect.stringContaining("Composer"),
		]);

		layout.scrollPage("up");
		const scrolledLines = pane.render(40).map(stripVTControlCharacters);
		expect(scrolledLines[0]).toContain("Sessions");
		expect(scrolledLines[1]).toContain("Session A");
		expect(scrolledLines.join("\n")).toContain("4");
		expect(scrolledLines.join("\n")).toContain("Composer");
	});

	it("restores a saved conversation offset without treating existing history as new output", () => {
		const layout = new ChatScrollLayout(
			new StaticComponent(["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]),
			new StaticComponent(["Composer"]),
			() => 5,
		);

		layout.render(40);
		layout.scrollPage("up");
		const savedOffset = layout.getScrollOffset();
		expect(savedOffset).toBeGreaterThan(0);

		layout.setScrollOffset(0);
		expect(layout.render(40).map(stripVTControlCharacters).join("\n")).toContain("8");

		layout.setScrollOffset(savedOffset);
		const restoredLines = layout.render(40).map(stripVTControlCharacters);
		expect(layout.getScrollOffset()).toBe(savedOffset);
		expect(restoredLines.join("\n")).toContain("4");
	});

	it("supports small line-by-line chat scrolling without changing page behavior", () => {
		const layout = new ChatScrollLayout(
			new StaticComponent(["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]),
			new StaticComponent(["Composer"]),
			() => 5,
		);

		layout.render(40);
		expect(layout.scrollLines("up")).toBe(true);
		expect(layout.getScrollOffset()).toBe(1);
		expect(layout.render(40).map(stripVTControlCharacters).join("\n")).toContain("5");

		expect(layout.scrollPage("up")).toBe(true);
		expect(layout.getScrollOffset()).toBe(4);
	});

	it("signals when upward navigation is close to the oldest loaded content", () => {
		const layout = new ChatScrollLayout(
			new StaticComponent(Array.from({ length: 30 }, (_, index) => String(index))),
			new StaticComponent(["Composer"]),
			() => 5,
		);

		layout.render(40);
		expect(layout.isNearOldestContent()).toBe(false);
		while (layout.scrollPage("up")) {
			if (layout.isNearOldestContent()) break;
		}
		expect(layout.isNearOldestContent()).toBe(true);
	});
});
