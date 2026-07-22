import type { AssistantMessage } from "@frelion/bone-ai";
import { type BoneRenderContext, type BoneView, createBoneTestRenderer } from "@frelion/bone-tui";
import { afterEach, describe, expect, test, vi } from "vitest";
import {
	OpenTUIAssistantMessage,
	OpenTUIPlanProposal,
	OpenTUIUserMessage,
} from "../src/modes/interactive/components/opentui-messages.ts";
import { OpenTUIInteractiveShell } from "../src/modes/interactive/opentui-shell.ts";
import { initTheme, theme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<Awaited<ReturnType<typeof createBoneTestRenderer>>>();

async function flushUntil(
	renderer: Awaited<ReturnType<typeof createBoneTestRenderer>>,
	predicate: (frame: string) => boolean,
): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await renderer.flush();
		const frame = renderer.captureFrame();
		if (predicate(frame)) return frame;
	}
	return renderer.captureFrame();
}

function textView(content: string): BoneView {
	return {
		mount(context: BoneRenderContext) {
			return context.createText({ content, fg: theme.getFgColor("text") });
		},
	};
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
	for (const renderer of renderers) renderer.destroy();
	renderers.clear();
});

describe("OpenTUI interactive shell", () => {
	test("lays out sidebar, structured transcript messages, and fixed chrome", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 100, height: 28 });
		renderers.add(renderer);
		renderer.start();

		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 24 });
		renderer.mount(shell);
		shell.setSidebar(textView("Conversations\ncurrent"));
		shell.appendTranscript(new OpenTUIUserMessage("Inspect this repository"));
		shell.appendTranscript(new OpenTUIAssistantMessage(assistantMessage("The repository uses Bun.")));
		shell.appendFixed(textView("Bun · OpenTUI"));

		const conversationFrame = await flushUntil(renderer, (frame) => frame.includes("The repository uses Bun."));
		expect(conversationFrame).toContain("Conversations");
		expect(conversationFrame).toContain("Inspect this repository");
		expect(conversationFrame).toContain("The repository uses Bun.");
		expect(conversationFrame).toContain("Bun · OpenTUI");

		shell.appendTranscript(
			new OpenTUIPlanProposal({
				id: "plan-1",
				version: 2,
				content: "# Migration\n\nMove the renderer to OpenTUI.",
				createdAt: "2026-07-22T00:00:00.000Z",
				sourceMessageId: "assistant-1",
			}),
		);

		const planFrame = await flushUntil(renderer, (frame) => frame.includes("Move the renderer to OpenTUI."));
		expect(planFrame).toContain("Plan v2");
		expect(planFrame).toContain("Move the renderer to OpenTUI.");
	});

	test("updates an assistant message in place without rebuilding the shell", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 80, height: 18 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell({ sidebarWidth: 20 });
		renderer.mount(shell);
		const createMarkdown = vi.spyOn(renderer, "createMarkdown");
		const assistant = new OpenTUIAssistantMessage(assistantMessage("first chunk"));
		shell.appendTranscript(assistant);

		const initialFrame = await flushUntil(renderer, (frame) => frame.includes("first chunk"));
		expect(initialFrame).toContain("first chunk");
		assistant.updateContent(assistantMessage("final response"));
		const finalFrame = await flushUntil(renderer, (frame) => frame.includes("final response"));

		expect(finalFrame).toContain("final response");
		expect(finalFrame).not.toContain("first chunk");
		expect(createMarkdown).toHaveBeenCalledTimes(1);
	});

	test("switches between split and single-pane layouts without remounting content", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 100, height: 24 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell();
		renderer.mount(shell);
		shell.setSidebar(textView("CONVERSATIONS\ncurrent"));
		shell.appendTranscript(textView("responsive transcript"));

		expect(await flushUntil(renderer, (frame) => frame.includes("responsive transcript"))).toContain("CONVERSATIONS");
		expect(shell.layoutMode).toBe("split");

		renderer.resize(70, 18);
		const compactMain = await flushUntil(renderer, (frame) => frame.includes("responsive transcript"));
		expect(shell.layoutMode).toBe("single");
		expect(compactMain).not.toContain("CONVERSATIONS");

		shell.showPane("sidebar");
		const compactSidebar = await flushUntil(renderer, (frame) => frame.includes("CONVERSATIONS"));
		expect(compactSidebar).not.toContain("responsive transcript");

		shell.showPane("main");
		expect(await flushUntil(renderer, (frame) => frame.includes("responsive transcript"))).not.toContain(
			"CONVERSATIONS",
		);
	});

	test("constrains sidebar resizing and emits persisted widths", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 120, height: 24 });
		renderers.add(renderer);
		renderer.start();
		const shell = new OpenTUIInteractiveShell();
		const onSidebarWidthChange = vi.fn();
		shell.onSidebarWidthChange = onSidebarWidthChange;
		renderer.mount(shell);
		shell.setSidebar(textView("CONVERSATIONS"));

		shell.setSidebarWidth(50, true);

		expect(shell.sidebarWidth).toBe(50);
		expect(shell.layoutMode).toBe("split");
		expect(onSidebarWidthChange).toHaveBeenLastCalledWith(50);

		shell.setSidebarWidth(90, true);
		expect(shell.sidebarWidth).toBe(60);
		expect(onSidebarWidthChange).toHaveBeenLastCalledWith(60);
	});
});
