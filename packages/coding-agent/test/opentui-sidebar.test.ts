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
import { OpenTUIInteractiveShell } from "../src/modes/interactive/opentui-shell.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<BoneTestRenderer>();

function makeSession(id: string, state: InteractiveSessionSummary["state"]): InteractiveSessionSummary {
	return {
		path: `/sessions/${id}.jsonl`,
		id,
		cwd: "/workspace",
		created: new Date(2026, 6, 22, 9, 41),
		modified: new Date(),
		messageCount: 3,
		firstMessage: `Session ${id}`,
		allMessagesText: `Session ${id}`,
		lastMessage: `Latest message for ${id}`,
		lastMessageRole: "assistant",
		state,
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
		expect(frame).toContain("Conversations");
		expect(frame).toContain("Session a");
		expect(frame).toContain("Session b");
		expect(frame).toContain("now · run");

		renderer.input.pressArrow("down");
		renderer.input.pressEnter();
		expect(activate).toHaveBeenCalledWith("/sessions/b.jsonl");

		renderer.input.pressKey("d");
		const confirmFrame = await flushUntil(renderer, "Delete this conversation?");
		expect(confirmFrame).toContain("Enter confirm · Esc cancel");
		renderer.input.pressEscape();
		renderer.input.pressEnter();
		expect(remove).not.toHaveBeenCalled();
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
		expect(searchFrame).toContain("Search conversations");
		expect(searchFrame).not.toContain("Session a");

		renderer.input.pressArrow("down");
		expect(preview).toHaveBeenLastCalledWith("/sessions/c.jsonl");
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
		const renderer = await createBoneTestRenderer({ width: 90, height: 18 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 30 });
		renderer.mount(shell);
		const sidebar = new OpenTUISessionSidebar();
		const sidebarNode = shell.setSidebar(sidebar);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		const activate = vi.fn();
		sidebar.onActivateSession = activate;
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
		renderer.input.pressEnter();
		expect(activate).toHaveBeenCalledWith("/sessions/b.jsonl");

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
});
