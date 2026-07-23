import type { AssistantMessage } from "@frelion/bone-ai";
import { type Renderable, TextRenderable } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test, vi } from "vitest";
import {
	OpenTUIAssistantMessage,
	OpenTUIPlanProposal,
	OpenTUIUserMessage,
} from "../src/modes/interactive/components/opentui-messages.ts";
import { OpenTUIInteractiveShell } from "../src/modes/interactive/opentui-shell.ts";
import { initTheme, theme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<TestRendererSetup>();

async function createRenderer(width: number, height: number): Promise<TestRendererSetup> {
	const setup = await createTestRenderer({ width, height, autoFocus: false, useMouse: true });
	renderers.add(setup);
	setup.renderer.start();
	return setup;
}

async function flushUntil(setup: TestRendererSetup, predicate: (frame: string) => boolean): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await setup.flush();
		const frame = setup.captureCharFrame();
		if (predicate(frame)) return frame;
	}
	return setup.captureCharFrame();
}

function textView(setup: TestRendererSetup, content: string): Renderable {
	return new TextRenderable(setup.renderer, { content, fg: theme.getFgColor("text") });
}

function assistantMessage(text: string): AssistantMessage {
	return {
		role: "assistant",
		content: [{ type: "text", text }],
		api: "openai-responses",
		provider: "openai",
		model: "gpt-4o-mini",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "stop",
		timestamp: Date.now(),
	};
}

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUI interactive shell", () => {
	test("lays out sidebar, structured transcript messages, and fixed chrome", async () => {
		initTheme("dark");
		const setup = await createRenderer(100, 28);
		const { renderer } = setup;

		const shell = new OpenTUIInteractiveShell(renderer, { sidebarWidth: 24 });
		renderer.root.add(shell.root);
		shell.setSidebar(textView(setup, "Conversations\ncurrent"));
		shell.appendTranscript(new OpenTUIUserMessage(renderer, "Inspect this repository").root);
		shell.appendTranscript(new OpenTUIAssistantMessage(renderer, assistantMessage("The repository uses Bun.")).root);
		shell.appendFixed(textView(setup, "Bun · OpenTUI"));

		const conversationFrame = await flushUntil(setup, (frame) => frame.includes("The repository uses Bun."));
		expect(conversationFrame).toContain("Conversations");
		expect(conversationFrame).toContain("Inspect this repository");
		expect(conversationFrame).toContain("The repository uses Bun.");
		expect(conversationFrame).toContain("Bun · OpenTUI");

		shell.appendTranscript(
			new OpenTUIPlanProposal(renderer, {
				id: "plan-1",
				version: 2,
				content: "# Migration\n\nMove the renderer to OpenTUI.",
				createdAt: "2026-07-22T00:00:00.000Z",
				sourceMessageId: "assistant-1",
			}).root,
		);

		const planFrame = await flushUntil(setup, (frame) => frame.includes("Move the renderer to OpenTUI."));
		expect(planFrame).toContain("Plan v2");
		expect(planFrame).toContain("Move the renderer to OpenTUI.");
	});

	test("updates an assistant message in place without rebuilding the shell", async () => {
		initTheme("dark");
		const setup = await createRenderer(80, 18);
		const { renderer } = setup;
		const shell = new OpenTUIInteractiveShell(renderer, { sidebarWidth: 20 });
		renderer.root.add(shell.root);
		const assistant = new OpenTUIAssistantMessage(renderer, assistantMessage("first chunk"));
		shell.appendTranscript(assistant.root);

		const initialFrame = await flushUntil(setup, (frame) => frame.includes("first chunk"));
		expect(initialFrame).toContain("first chunk");
		const renderedMessage = assistant.root.getChildren().at(-1);
		assistant.updateContent(assistantMessage("final response"));
		const finalFrame = await flushUntil(setup, (frame) => frame.includes("final response"));

		expect(finalFrame).toContain("final response");
		expect(finalFrame).not.toContain("first chunk");
		expect(assistant.root.getChildren().at(-1)).toBe(renderedMessage);
	});

	test("keeps the transcript viewport and scrollbar in one continuous region", async () => {
		initTheme("dark");
		const setup = await createRenderer(100, 40);
		const { renderer } = setup;
		const shell = new OpenTUIInteractiveShell(renderer, { sidebarWidth: 32 });
		renderer.root.add(shell.root);

		let lastLine: ReturnType<OpenTUIInteractiveShell["appendTranscript"]> | undefined;
		for (let index = 0; index < 50; index++) {
			lastLine = shell.appendTranscript(textView(setup, `line-${index}`));
		}
		const transcript = shell.getTranscriptNode();
		transcript.scrollTo(Number.MAX_SAFE_INTEGER);
		await flushUntil(setup, (frame) => frame.includes("line-49"));

		expect(lastLine).toBeDefined();
		expect(lastLine!.screenY + lastLine!.height).toBe(transcript.screenY + transcript.height);
	});

	test("switches between split and single-pane layouts without remounting content", async () => {
		initTheme("dark");
		const setup = await createRenderer(100, 24);
		const { renderer } = setup;
		const shell = new OpenTUIInteractiveShell(renderer);
		renderer.root.add(shell.root);
		shell.setSidebar(textView(setup, "CONVERSATIONS\ncurrent"));
		shell.appendTranscript(textView(setup, "responsive transcript"));

		expect(await flushUntil(setup, (frame) => frame.includes("responsive transcript"))).toContain("CONVERSATIONS");
		expect(shell.layoutMode).toBe("split");

		setup.resize(70, 18);
		const compactMain = await flushUntil(setup, (frame) => frame.includes("responsive transcript"));
		expect(shell.layoutMode).toBe("single");
		expect(compactMain).not.toContain("CONVERSATIONS");

		shell.showPane("sidebar");
		const compactSidebar = await flushUntil(setup, (frame) => frame.includes("CONVERSATIONS"));
		expect(compactSidebar).not.toContain("responsive transcript");

		shell.showPane("main");
		expect(await flushUntil(setup, (frame) => frame.includes("responsive transcript"))).not.toContain(
			"CONVERSATIONS",
		);
	});

	test("constrains sidebar resizing and emits persisted widths", async () => {
		initTheme("dark");
		const setup = await createRenderer(120, 24);
		const { renderer } = setup;
		const shell = new OpenTUIInteractiveShell(renderer);
		const onSidebarWidthChange = vi.fn();
		shell.onSidebarWidthChange = onSidebarWidthChange;
		renderer.root.add(shell.root);
		shell.setSidebar(textView(setup, "CONVERSATIONS"));

		shell.setSidebarWidth(50, true);

		expect(shell.sidebarWidth).toBe(50);
		expect(shell.layoutMode).toBe("split");
		expect(onSidebarWidthChange).toHaveBeenLastCalledWith(50);

		shell.setSidebarWidth(90, true);
		expect(shell.sidebarWidth).toBe(60);
		expect(onSidebarWidthChange).toHaveBeenLastCalledWith(60);
	});

	test("keeps resizing after the pointer leaves the one-column separator", async () => {
		initTheme("dark");
		const setup = await createRenderer(120, 24);
		const { renderer } = setup;
		const shell = new OpenTUIInteractiveShell(renderer, { sidebarWidth: 38 });
		const onSidebarWidthChange = vi.fn();
		shell.onSidebarWidthChange = onSidebarWidthChange;
		renderer.root.add(shell.root);
		shell.setSidebar(textView(setup, "CONVERSATIONS"));
		await setup.flush();

		await setup.mockMouse.drag(38, 5, 50, 5);
		await setup.flush();

		expect(shell.sidebarWidth).toBe(50);
		expect(onSidebarWidthChange).toHaveBeenLastCalledWith(50);
	});
});
