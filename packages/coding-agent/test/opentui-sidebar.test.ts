import { type KeyEvent, TextareaRenderable, TextRenderable } from "@opentui/core";
import { createTestRenderer, MouseButtons, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import type { InteractiveSessionSummary } from "../src/core/interactive-session-host.ts";
import { OpenTUISessionSidebar } from "../src/modes/interactive/components/opentui-session-sidebar.ts";
import { OpenTUITranscriptFocusController } from "../src/modes/interactive/components/opentui-transcript-focus.ts";
import { OpenTUIPaneNavigator } from "../src/modes/interactive/components/pane-navigator.ts";
import { OpenTUIInteractiveShell } from "../src/modes/interactive/opentui-shell.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<TestRendererSetup>();

async function createRenderer(width: number, height: number): Promise<TestRendererSetup> {
	const setup = await createTestRenderer({ width, height, autoFocus: false, useMouse: true, kittyKeyboard: true });
	renderers.add(setup);
	setup.renderer.start();
	return setup;
}

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

function lineView(renderer: TestRendererSetup["renderer"], content: string) {
	return new TextRenderable(renderer, { content, height: 1 });
}

async function flushUntil(setup: TestRendererSetup, text: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await setup.flush();
		const frame = setup.captureCharFrame();
		if (frame.includes(text)) return frame;
	}
	return setup.captureCharFrame();
}

beforeEach(() => {
	initTheme("dark");
});

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUI session sidebar", () => {
	test("renders session details and routes structured keyboard actions", async () => {
		const setup = await createRenderer(100, 24);
		const { renderer, mockInput } = setup;
		const shell = new OpenTUIInteractiveShell(renderer, { sidebarWidth: 34 });
		renderer.root.add(shell.root);
		const sidebar = new OpenTUISessionSidebar(renderer);
		const sidebarNode = shell.setSidebar(sidebar.root);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		const activate = vi.fn();
		const remove = vi.fn();
		sidebar.onActivateSession = activate;
		sidebar.onDeleteSession = remove;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "background-running")]);
		const focus = new OpenTUIPaneNavigator(renderer);
		focus.register("sidebar", {
			root: sidebarNode,
			focusTarget: () => sidebar.focusTarget,
			onFocusChange: (focused) => sidebar.setFocused(focused),
		});
		sidebar.onSearchStateChange = () => focus.focus("sidebar");
		focus.focus("sidebar");
		const routeKeys = (event: KeyEvent) => {
			if (focus.focusedPane === "sidebar") sidebar.handleKey(event);
		};
		renderer.keyInput.on("keypress", routeKeys);

		const frame = await flushUntil(setup, "Latest message for b");
		expect(frame).toContain("CONVERSATIONS");
		expect(frame).toContain("Session a");
		expect(frame).toContain("Session b");
		expect(frame).toContain("↻");
		expect(frame).toContain("3 msgs");
		expect(frame).toContain("↑↓ select    / search");
		expect(frame).toContain("d delete     ↵ open");
		expect(frame).not.toContain("You ·");
		expect(frame).not.toContain("Bone ·");

		mockInput.pressArrow("down");
		mockInput.pressEnter();
		expect(activate).toHaveBeenCalledWith("/sessions/b.jsonl");

		mockInput.pressKey("d");
		const confirmFrame = await flushUntil(setup, "Press d again to delete");
		expect(confirmFrame).toContain("Press d again to delete");
		mockInput.pressEnter();
		expect(remove).not.toHaveBeenCalled();
		mockInput.pressKey("d");
		expect(remove).toHaveBeenCalledWith("/sessions/b.jsonl", "/sessions/a.jsonl");
		renderer.keyInput.off("keypress", routeKeys);
		focus.dispose();
	});

	test("keeps search selection local until the user explicitly opens a result", async () => {
		const setup = await createRenderer(100, 24);
		const { renderer, mockInput } = setup;
		const shell = new OpenTUIInteractiveShell(renderer, { sidebarWidth: 38 });
		renderer.root.add(shell.root);
		const sidebar = new OpenTUISessionSidebar(renderer);
		const sidebarNode = shell.setSidebar(sidebar.root);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		const queryChange = vi.fn();
		const preview = vi.fn();
		const activate = vi.fn();
		sidebar.onSearchQueryChange = queryChange;
		sidebar.onPreviewSession = preview;
		sidebar.onActivateSession = activate;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "cold"), makeSession("c", "cold")]);
		const focus = new OpenTUIPaneNavigator(renderer);
		focus.register("sidebar", {
			root: sidebarNode,
			focusTarget: () => sidebar.focusTarget,
			onFocusChange: (focused) => sidebar.setFocused(focused),
		});
		sidebar.onSearchStateChange = () => focus.focus("sidebar");
		focus.focus("sidebar");
		const routeKeys = (event: KeyEvent) => {
			if (focus.focusedPane === "sidebar") sidebar.handleKey(event);
		};
		renderer.keyInput.on("keypress", routeKeys);

		mockInput.pressKey("/");
		await mockInput.typeText("semantic");
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
		const searchFrame = await flushUntil(setup, "semantic search");
		expect(searchFrame).toContain("CONVERSATIONS");
		expect(searchFrame).toContain("↑↓ results   type to search");
		expect(searchFrame).toContain("↵ open       esc cancel");
		expect(searchFrame).not.toContain("Session a");

		mockInput.pressArrow("down");
		expect(preview).not.toHaveBeenCalled();
		expect(activate).not.toHaveBeenCalled();
		focus.focus("sidebar");
		await mockInput.typeText(" cache");
		expect(sidebar.searchQuery).toBe("semantic cache");
		mockInput.pressEscape();
		expect(preview).not.toHaveBeenCalled();
		expect(activate).not.toHaveBeenCalled();
		expect(sidebar.searchActive).toBe(false);

		mockInput.pressKey("/");
		await mockInput.typeText("semantic");
		sidebar.setSearchResults([
			{
				sessionPath: "/sessions/b.jsonl",
				score: 2,
				evidence: { kind: "user", label: "You", snippet: "semantic cache" },
			},
		]);
		mockInput.pressEnter();
		expect(activate).toHaveBeenCalledWith("/sessions/b.jsonl");
		renderer.keyInput.off("keypress", routeKeys);
		focus.dispose();
	});

	test("filters loaded conversations before richer search results arrive", async () => {
		const setup = await createRenderer(100, 24);
		const { renderer, mockInput } = setup;
		const shell = new OpenTUIInteractiveShell(renderer, { sidebarWidth: 38 });
		renderer.root.add(shell.root);
		const sidebar = new OpenTUISessionSidebar(renderer);
		const sidebarNode = shell.setSidebar(sidebar.root);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		sidebar.setSessions([makeSession("alpha", "foreground"), makeSession("beta", "cold")]);
		const focus = new OpenTUIPaneNavigator(renderer);
		focus.register("sidebar", {
			root: sidebarNode,
			focusTarget: () => sidebar.focusTarget,
			onFocusChange: (focused) => sidebar.setFocused(focused),
		});
		sidebar.onSearchStateChange = () => focus.focus("sidebar");
		focus.focus("sidebar");
		const routeKeys = (event: KeyEvent) => {
			if (focus.focusedPane === "sidebar") sidebar.handleKey(event);
		};
		renderer.keyInput.on("keypress", routeKeys);

		mockInput.pressKey("/");
		await mockInput.typeText("beta");
		const frame = await flushUntil(setup, "Session beta");
		expect(frame).not.toContain("Session alpha");
		renderer.keyInput.off("keypress", routeKeys);
		focus.dispose();
	});

	test("supports mouse selection while native focus and transcript scrolling remain independent", async () => {
		const setup = await createRenderer(100, 18);
		const { renderer, mockInput, mockMouse } = setup;
		const shell = new OpenTUIInteractiveShell(renderer, { sidebarWidth: 30 });
		renderer.root.add(shell.root);
		const sidebar = new OpenTUISessionSidebar(renderer);
		const sidebarNode = shell.setSidebar(sidebar.root);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		const activate = vi.fn();
		const focusChat = vi.fn();
		sidebar.onActivateSession = activate;
		sidebar.setSessions([makeSession("a", "foreground"), makeSession("b", "cold")]);
		for (let index = 0; index < 30; index++) shell.appendTranscript(lineView(renderer, `transcript-${index}`));
		const composer = new TextareaRenderable(renderer, { width: "100%", height: 1 });
		shell.appendFixed(composer);

		const focus = new OpenTUIPaneNavigator(renderer);
		focus.register("sidebar", {
			root: sidebarNode,
			focusTarget: () => sidebar.focusTarget,
			onFocusChange: (focused) => sidebar.setFocused(focused),
		});
		focus.register("composer", { root: composer, focusTarget: composer });
		const history = new OpenTUITranscriptFocusController(shell.getTranscriptNode(), () => renderer.height);
		const focusComposer = () => {
			focusChat();
			focus.focus("composer");
		};
		sidebar.onFocusChat = focusComposer;
		sidebar.onScrollChat = (direction) => history.scrollByUser(direction === "up" ? -10 : 10);
		shell.onTranscriptFocusRequest = focusComposer;
		focus.focus("sidebar");
		const routeKeys = (event: KeyEvent) => {
			if (focus.focusedPane === "sidebar") sidebar.handleKey(event);
		};
		renderer.keyInput.on("keypress", routeKeys);
		await flushUntil(setup, "transcript-29");

		const rows = sidebar.list.getChildren().filter((child) => child.height === 3);
		const firstRow = rows[0];
		const secondRow = rows[1];
		if (!firstRow || !secondRow) throw new Error("Expected two session rows");
		const firstPoint = { x: firstRow.screenX + 2, y: firstRow.screenY + 1 };
		const secondPoint = { x: secondRow.screenX + 2, y: secondRow.screenY + 1 };
		const pointerChanges = vi.spyOn(renderer, "setMousePointer");
		const renderRequests = vi.spyOn(renderer, "requestRender");
		const restingBackground = secondRow.backgroundColor.toString();
		await mockMouse.moveTo(secondPoint.x, secondPoint.y);
		expect(pointerChanges).toHaveBeenLastCalledWith("pointer");
		expect(renderRequests).toHaveBeenCalled();
		const normalHoverBackground = secondRow.backgroundColor.toString();
		expect(normalHoverBackground).not.toBe(restingBackground);
		await mockMouse.moveTo(shell.mainRoot.screenX + 2, shell.mainRoot.screenY + 2);
		expect(pointerChanges).toHaveBeenLastCalledWith("default");
		expect(secondRow.backgroundColor.toString()).toBe(restingBackground);
		sidebar.setSessions([makeSession("a", "cold"), makeSession("b", "foreground")]);
		await mockMouse.moveTo(secondPoint.x, secondPoint.y);
		expect(secondRow.backgroundColor.toString()).not.toBe(normalHoverBackground);
		await mockMouse.moveTo(shell.mainRoot.screenX + 2, shell.mainRoot.screenY + 2);

		await mockMouse.pressDown(secondPoint.x, secondPoint.y);
		expect(activate).not.toHaveBeenCalled();
		expect(focusChat).not.toHaveBeenCalled();
		await mockMouse.release(secondPoint.x, secondPoint.y);
		expect(activate).toHaveBeenCalledWith("/sessions/b.jsonl");
		expect(focusChat).toHaveBeenCalledOnce();
		expect(focus.focusedPane).toBe("composer");
		expect(renderer.currentFocusedRenderable).toBe(composer);

		activate.mockClear();
		focusChat.mockClear();
		focus.focus("sidebar");
		await mockMouse.drag(secondPoint.x, secondPoint.y, firstPoint.x, firstPoint.y);
		expect(activate).not.toHaveBeenCalled();
		expect(focusChat).not.toHaveBeenCalled();
		await mockMouse.click(secondPoint.x, secondPoint.y, MouseButtons.RIGHT);
		expect(activate).not.toHaveBeenCalled();
		expect(focusChat).not.toHaveBeenCalled();
		await mockMouse.pressDown(secondPoint.x, secondPoint.y);
		await mockMouse.release(firstPoint.x, firstPoint.y);
		expect(activate).not.toHaveBeenCalled();
		expect(focusChat).not.toHaveBeenCalled();
		await mockMouse.pressDown(secondPoint.x, secondPoint.y);
		sidebar.setStatusMessage("Session changed while pressed");
		await setup.flush();
		await mockMouse.release(secondPoint.x, secondPoint.y);
		expect(activate).not.toHaveBeenCalled();
		expect(focusChat).not.toHaveBeenCalled();
		sidebar.setStatusMessage(undefined);
		await setup.flush();
		await mockMouse.click(secondPoint.x, secondPoint.y);
		expect(activate).toHaveBeenCalledOnce();
		expect(activate).toHaveBeenCalledWith("/sessions/b.jsonl");
		expect(focusChat).toHaveBeenCalledOnce();

		focus.focus("sidebar");
		await mockMouse.click(50, 8);
		expect(focus.focusedPane).toBe("sidebar");
		expect(renderer.currentFocusedRenderable).toBe(sidebar.focusTarget);
		const before = shell.getTranscriptNode().scrollTop;
		mockInput.pressKey("\x1b[5~");
		await setup.flush();
		expect(shell.getTranscriptNode().scrollTop).toBeLessThan(before);
		mockInput.pressArrow("right", { shift: true });
		expect(focus.focusedPane).toBe("composer");
		expect(renderer.currentFocusedRenderable).toBe(composer);
		renderer.keyInput.off("keypress", routeKeys);
		focus.dispose();
	});

	test("sorts by activity, freezes order while focused, and reorders once on blur", async () => {
		const setup = await createRenderer(100, 24);
		const { renderer } = setup;
		const shell = new OpenTUIInteractiveShell(renderer, { sidebarWidth: 38 });
		renderer.root.add(shell.root);
		const sidebar = new OpenTUISessionSidebar(renderer);
		const sidebarNode = shell.setSidebar(sidebar.root);
		if (!sidebarNode) throw new Error("Expected sidebar node");
		const older = makeSession("older", "foreground", { modified: new Date("2026-07-22T09:00:00Z") });
		const newer = makeSession("newer", "cold", { modified: new Date("2026-07-22T10:00:00Z") });
		sidebar.setSessions([older, newer]);
		sidebar.setFocused(true);

		let frame = await flushUntil(setup, "Session newer");
		expect(frame.indexOf("Session newer")).toBeLessThan(frame.indexOf("Session older"));

		sidebar.setSessions([
			{ ...older, modified: new Date("2026-07-22T11:00:00Z"), lastMessage: "new completed activity" },
			newer,
		]);
		frame = await flushUntil(setup, "new completed activity");
		expect(frame.indexOf("Session newer")).toBeLessThan(frame.indexOf("Session older"));

		sidebar.setFocused(false);
		frame = await flushUntil(setup, "Session older");
		expect(frame.indexOf("Session older")).toBeLessThan(frame.indexOf("Session newer"));
	});

	test("keeps running rows stable while live output and throughput change", async () => {
		const setup = await createRenderer(100, 18);
		const { renderer } = setup;
		const shell = new OpenTUIInteractiveShell(renderer, { sidebarWidth: 38 });
		renderer.root.add(shell.root);
		const sidebar = new OpenTUISessionSidebar(renderer);
		shell.setSidebar(sidebar.root);
		sidebar.setSessions([
			makeSession("live", "background-running", {
				lastMessage: "Last completed response",
				livePreview: "first streaming fragment",
				throughputTokensPerSecond: 12.34,
			}),
		]);

		await flushUntil(setup, "Last completed response");
		const initial = setup.captureCharFrame();
		const firstRow = sidebar.list.getChildren()[0];
		expect(initial).toContain("↻");
		expect(initial).not.toContain("first streaming fragment");
		expect(initial).not.toContain("tok/s");

		sidebar.setSessions([
			makeSession("live", "background-running", {
				lastMessage: "Last completed response",
				livePreview: "newest streaming fragment",
				throughputTokensPerSecond: 98.76,
			}),
		]);
		await setup.flush();
		expect(sidebar.list.getChildren()[0]).toBe(firstRow);
		const after = setup.captureCharFrame();
		expect(after).toBe(initial);
		expect(after).not.toContain("newest streaming fragment");
		expect(after).not.toContain("98.8 tok/s");
	});
});
