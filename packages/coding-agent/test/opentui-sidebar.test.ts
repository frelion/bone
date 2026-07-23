import {
	type BoneRenderContext,
	type BoneTestRenderer,
	type BoneView,
	createBoneTestRenderer,
} from "@frelion/bone-tui";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { InteractiveSessionSummary } from "../src/core/interactive-session-host.ts";
import { OpenTUISessionSidebar } from "../src/modes/interactive/components/opentui-session-sidebar.ts";
import { OpenTUITranscriptFocusController } from "../src/modes/interactive/components/opentui-transcript-focus.ts";
import { OpenTUIPaneFocusController } from "../src/modes/interactive/components/pane-focus-controller.ts";
import { OPEN_TUI_LAYOUT } from "../src/modes/interactive/opentui-design.ts";
import { OpenTUIInteractiveShell } from "../src/modes/interactive/opentui-shell.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<BoneTestRenderer>();

function makeSession(
	id: string,
	state: InteractiveSessionSummary["state"],
	overrides: Partial<InteractiveSessionSummary> = {},
): InteractiveSessionSummary {
	return {
		path: `/sessions/${id}.jsonl`,
		id,
		cwd: "/workspace",
		created: new Date(2026, 6, 22, 9, 41),
		modified: new Date(2026, 6, 22, 9, 45),
		messageCount: 3,
		firstMessage: `Session ${id}`,
		allMessagesText: `Session ${id}`,
		lastMessage: `Latest message for ${id}`,
		lastMessageRole: "assistant",
		state,
		...overrides,
	};
}

function lineView(content: string): BoneView {
	return {
		mount(context: BoneRenderContext) {
			return context.createText({ content, height: 1 });
		},
	};
}

async function flushUntil(renderer: BoneTestRenderer, text: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await renderer.flush();
		const frame = renderer.captureFrame();
		if (frame.includes(text)) return frame;
	}
	return renderer.captureFrame();
}

beforeEach(() => {
	initTheme("dark");
});

afterEach(() => {
	for (const renderer of renderers) renderer.destroy();
	renderers.clear();
});

describe("OpenTUI session sidebar", () => {
	test("renders session details and routes structured keyboard actions", async () => {
		const renderer = await createBoneTestRenderer({ width: 100, height: 24 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 34 });
		renderer.mount(shell);
		const sidebar = new OpenTUISessionSidebar();
		const sidebarNode = shell.setSidebar(sidebar);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		const activate = vi.fn();
		const remove = vi.fn();
		sidebar.onActivateSession = activate;
		sidebar.onDeleteSession = remove;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "background-running")]);
		const focus = new OpenTUIPaneFocusController(renderer);
		focus.register("sidebar", {
			node: sidebarNode,
			handleKey: (event) => sidebar.handleKey(event),
			onFocusChange: (focused) => sidebar.setFocused(focused),
		});
		focus.focus("sidebar");

		const frame = await flushUntil(renderer, "Latest message for b");
		expect(frame).toContain("CONVERSATIONS");
		expect(frame).toContain("Session a");
		expect(frame).toContain("Session b");
		expect(frame).toContain("↻");
		expect(frame).toContain("3 msgs");
		expect(frame).toContain("↑↓ select    / search");
		expect(frame).toContain("d delete     ↵ open");
		expect(frame).not.toContain("You ·");
		expect(frame).not.toContain("Bone ·");

		renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		expect(activate).toHaveBeenCalledWith("/sessions/b.jsonl");

		renderer.input.pressKey("d");
		const confirmFrame = await flushUntil(renderer, "Press d again to delete");
		expect(confirmFrame).toContain("Press d again to delete");
		renderer.input.pressEnter();
		expect(remove).not.toHaveBeenCalled();
		renderer.input.pressKey("d");
		expect(remove).toHaveBeenCalledWith("/sessions/b.jsonl", "/sessions/a.jsonl");
		focus.dispose();
	});

	test("keeps search live, previews results, and restores the original session", async () => {
		const renderer = await createBoneTestRenderer({ width: 100, height: 24 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 38 });
		renderer.mount(shell);
		const sidebar = new OpenTUISessionSidebar();
		const sidebarNode = shell.setSidebar(sidebar);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		const queryChange = vi.fn();
		const preview = vi.fn();
		sidebar.onSearchQueryChange = queryChange;
		sidebar.onPreviewSession = preview;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "cold"), makeSession("c", "cold")]);
		const focus = new OpenTUIPaneFocusController(renderer);
		focus.register("sidebar", {
			node: sidebarNode,
			handleKey: (event) => sidebar.handleKey(event),
			onFocusChange: (focused) => sidebar.setFocused(focused),
		});
		focus.focus("sidebar");

		renderer.input.pressKey("/");
		await renderer.input.typeText("semantic");
		expect(sidebar.searchQuery).toBe("semantic");
		expect(queryChange).toHaveBeenLastCalledWith("semantic");
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
		const searchFrame = await flushUntil(renderer, "semantic search");
		expect(searchFrame).toContain("CONVERSATIONS");
		expect(searchFrame).toContain("↑↓ results   type to search");
		expect(searchFrame).toContain("↵ open       esc cancel");
		expect(searchFrame).not.toContain("Session a");

		renderer.input.pressArrow("down");
		expect(preview).toHaveBeenLastCalledWith("/sessions/c.jsonl");
		focus.focus("sidebar");
		await renderer.input.typeText(" cache");
		expect(sidebar.searchQuery).toBe("semantic cache");
		renderer.input.pressEscape();
		expect(preview).toHaveBeenLastCalledWith("/sessions/a.jsonl");
		expect(sidebar.searchActive).toBe(false);
		focus.dispose();
	});

	test("filters loaded conversations before richer search results arrive", async () => {
		const renderer = await createBoneTestRenderer({ width: 100, height: 24 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 38 });
		renderer.mount(shell);
		const sidebar = new OpenTUISessionSidebar();
		const sidebarNode = shell.setSidebar(sidebar);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		sidebar.setSessions([makeSession("alpha", "foreground"), makeSession("beta", "cold")]);
		const focus = new OpenTUIPaneFocusController(renderer);
		focus.register("sidebar", {
			node: sidebarNode,
			handleKey: (event) => sidebar.handleKey(event),
			onFocusChange: (focused) => sidebar.setFocused(focused),
		});
		focus.focus("sidebar");

		renderer.input.pressKey("/");
		await renderer.input.typeText("beta");
		const frame = await flushUntil(renderer, "Session beta");
		expect(frame).not.toContain("Session alpha");
		focus.dispose();
	});

	test("supports mouse selection and transcript pane scrolling/focus", async () => {
		const renderer = await createBoneTestRenderer({ width: 100, height: 18 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 30 });
		renderer.mount(shell);
		const sidebar = new OpenTUISessionSidebar();
		const sidebarNode = shell.setSidebar(sidebar);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		const activate = vi.fn();
		const focusChat = vi.fn();
		sidebar.onActivateSession = activate;
		sidebar.onFocusChat = focusChat;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "cold")]);
		for (let index = 0; index < 30; index++) shell.appendTranscript(lineView(`transcript-${index}`));

		const focus = new OpenTUIPaneFocusController(renderer);
		focus.register("sidebar", {
			node: sidebarNode,
			handleKey: (event) => sidebar.handleKey(event),
			onFocusChange: (focused) => sidebar.setFocused(focused),
		});
		const history = new OpenTUITranscriptFocusController(shell.getTranscriptNode(), () => renderer.height);
		history.onFocusSidebar = () => focus.focus("sidebar");
		focus.register("history", history.toPane());
		shell.onTranscriptFocusRequest = () => focus.focus("history");
		focus.focus("sidebar");
		await flushUntil(renderer, "transcript-29");

		await renderer.mouse.click(5, 7);
		expect(activate).toHaveBeenCalledWith("/sessions/b.jsonl");
		expect(focusChat).toHaveBeenCalledOnce();

		await renderer.mouse.click(50, 8);
		expect(focus.focusedPane).toBe("history");
		const before = shell.getTranscriptNode().scrollTop;
		renderer.input.pressKey("\x1b[5~");
		await renderer.flush();
		expect(shell.getTranscriptNode().scrollTop).toBeLessThan(before);
		renderer.input.pressArrow("left", { shift: true });
		expect(focus.focusedPane).toBe("sidebar");
		focus.dispose();
	});

	test("sorts by activity, freezes order while focused, and reorders once on blur", async () => {
		const renderer = await createBoneTestRenderer({ width: 100, height: 24 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 38 });
		renderer.mount(shell);
		const sidebar = new OpenTUISessionSidebar();
		const sidebarNode = shell.setSidebar(sidebar);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		const older = makeSession("older", "foreground", { modified: new Date("2026-07-22T09:00:00Z") });
		const newer = makeSession("newer", "cold", { modified: new Date("2026-07-22T10:00:00Z") });
		sidebar.setSessions([older, newer]);
		sidebar.setFocused(true);

		let frame = await flushUntil(renderer, "Session newer");
		expect(frame.indexOf("Session newer")).toBeLessThan(frame.indexOf("Session older"));

		sidebar.setSessions([
			{ ...older, modified: new Date("2026-07-22T11:00:00Z"), livePreview: "new activity while focused" },
			newer,
		]);
		frame = await flushUntil(renderer, "new activity while focused");
		expect(frame.indexOf("Session newer")).toBeLessThan(frame.indexOf("Session older"));

		sidebar.setFocused(false);
		frame = await flushUntil(renderer, "Session older");
		expect(frame.indexOf("Session older")).toBeLessThan(frame.indexOf("Session newer"));
	});

	test("renders live throughput and keeps a running preview pinned to its newest output", async () => {
		const renderer = await createBoneTestRenderer({ width: 100, height: 18 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 38 });
		renderer.mount(shell);
		const sidebar = new OpenTUISessionSidebar();
		shell.setSidebar(sidebar);
		const preview = "This is a deliberately long live preview that keeps moving";
		sidebar.setSessions([
			makeSession("live", "background-running", {
				livePreview: preview,
				throughputTokensPerSecond: 12.34,
			}),
		]);

		await flushUntil(renderer, "12.3 tok/s");
		await new Promise((resolve) => setTimeout(resolve, OPEN_TUI_LAYOUT.marqueeIntervalMs + 40));
		await renderer.flush();
		expect(renderer.captureFrame()).toContain("keeps moving");

		sidebar.setSessions([
			makeSession("live", "background-running", {
				livePreview: `${preview} with newest streamed tokens`,
				throughputTokensPerSecond: 12.34,
			}),
		]);
		await new Promise((resolve) => setTimeout(resolve, OPEN_TUI_LAYOUT.marqueeIntervalMs + 40));
		await renderer.flush();
		const after = renderer.captureFrame();
		expect(after).toContain("newest streamed tokens");
		expect(after).not.toContain("This is a deliberately");
	});

	test("uses measured preview width and never loops old output back into a running conversation", async () => {
		const renderer = await createBoneTestRenderer({ width: 100, height: 18 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 32 });
		renderer.mount(shell);
		const sidebar = new OpenTUISessionSidebar();
		shell.setSidebar(sidebar);
		sidebar.setSessions([
			makeSession("measured", "background-running", {
				livePreview: "ABCDEFGHIJKLMNOPQRSTUV",
				throughputTokensPerSecond: 12.34,
			}),
		]);

		await flushUntil(renderer, "12.3 tok/s");
		await new Promise((resolve) => setTimeout(resolve, OPEN_TUI_LAYOUT.marqueeIntervalMs + 40));
		await renderer.flush();
		const after = renderer.captureFrame();
		expect(after).toContain("12.3 tok/s");
		expect(after).toContain("PQRSTUV");
		expect(after).not.toContain("ABCDEF");

		sidebar.dispose();
		await new Promise((resolve) => setTimeout(resolve, OPEN_TUI_LAYOUT.marqueeIntervalMs + 40));
		await renderer.flush();
		expect(renderer.captureFrame()).toBe(after);
	});

	test("keeps emoji output intact while following the latest preview tail", async () => {
		const renderer = await createBoneTestRenderer({ width: 100, height: 18 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 32 });
		renderer.mount(shell);
		const sidebar = new OpenTUISessionSidebar();
		shell.setSidebar(sidebar);
		sidebar.setSessions([
			makeSession("emoji", "background-running", {
				livePreview: "🙂ABCDEFGHIJKLMNOPQRSTUVWXYZ",
				throughputTokensPerSecond: 12.34,
			}),
		]);

		await flushUntil(renderer, "12.3 tok/s");
		await new Promise((resolve) => setTimeout(resolve, OPEN_TUI_LAYOUT.marqueeIntervalMs + 40));
		await renderer.flush();
		const frame = renderer.captureFrame();
		expect(frame).toContain("TUVWXYZ");
		expect(frame).not.toContain("�");
		sidebar.dispose();
	});
});
