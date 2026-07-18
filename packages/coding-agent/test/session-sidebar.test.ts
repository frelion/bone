import { stripVTControlCharacters } from "node:util";
import { setKeybindings } from "@earendil-works/pi-tui";
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
		expect(stripVTControlCharacters(sidebar.render(30).join("\n"))).toContain("› ○ Session b");
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
		expect(normalLines[0]?.trimEnd()).toMatch(/^ Conversations\s+1$/);
		expect(normalLines.join("\n")).not.toContain("Shift+→");
		expect(normalLines.join("\n")).not.toContain("d delete");

		sidebar.handleInput("d");
		const confirmationLines = stripVTControlCharacters(sidebar.render(42).join("\n")).split("\n");
		expect(confirmationLines[0]?.trimEnd()).toMatch(/^ Conversations\s+1$/);
		expect(confirmationLines.join("\n")).toContain("Delete this conversation?");
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
});

describe("SplitPane", () => {
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
});
