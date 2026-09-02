import type { AssistantMessage } from "@frelion/bone-ai";
import {
	CodeRenderable,
	MarkdownRenderable,
	type Renderable,
	TextAttributes,
	TextTableRenderable,
} from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test } from "vitest";
import {
	isOpenTUICommentaryText,
	OpenTUIAssistantMessage,
	OpenTUIUserMessage,
} from "../src/modes/interactive/components/opentui-messages.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<TestRendererSetup>();

async function flushUntil(setup: TestRendererSetup, text: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await setup.flush();
		const frame = setup.captureCharFrame();
		if (frame.includes(text)) return frame;
	}
	return setup.captureCharFrame();
}

async function settleCodeHighlighting(renderable: Renderable): Promise<void> {
	const pending: Promise<void>[] = [];
	const visit = (node: Renderable): void => {
		if (node instanceof CodeRenderable) pending.push(node.highlightingDone);
		for (const child of node.getChildren()) visit(child);
	};
	visit(renderable);
	await Promise.all(pending);
}

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

function assistant(
	content: AssistantMessage["content"],
	stopReason?: AssistantMessage["stopReason"],
): AssistantMessage {
	return {
		role: "assistant",
		content,
		api: "openai-responses",
		provider: "openai",
		model: "test",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason,
		timestamp: 1,
	};
}

describe("OpenTUI transcript messages", () => {
	test("recognizes commentary through normalized text phase metadata", () => {
		expect(
			isOpenTUICommentaryText({
				type: "text",
				text: "Inspecting the event queue",
				textSignature: JSON.stringify({ v: 1, id: "commentary-1", phase: "commentary" }),
			}),
		).toBe(true);
		expect(
			isOpenTUICommentaryText({
				type: "text",
				text: "Malformed metadata",
				textSignature: JSON.stringify({ v: 1, phase: "commentary" }),
			}),
		).toBe(false);
	});

	test("distinguishes user prompts from a label-free assistant stream", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 80, height: 16 });
		renderers.add(setup);
		const { renderer } = setup;
		const user = new OpenTUIUserMessage(renderer, "inspect this repository");
		renderer.root.add(user.root);
		const response = new OpenTUIAssistantMessage(renderer, assistant([{ type: "text", text: "Reading the files" }]));
		renderer.root.add(response.root);
		await setup.flush();
		const captured = setup.captureCharFrame();
		expect(captured).toContain("inspect this repository");
		expect(captured).toContain("Reading the files");
		expect(captured.toLowerCase()).not.toContain("you  inspect");
		expect(captured.toLowerCase()).not.toContain("bone  inspect");
		const body = user.root.getChildren()[1];
		expect(body?.backgroundColor).toBeDefined();
	});

	test("shows thinking only while the assistant is running", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 80, height: 12 });
		renderers.add(setup);
		const { renderer } = setup;
		const response = new OpenTUIAssistantMessage(
			renderer,
			assistant([{ type: "thinking", thinking: "Checking the dependency graph" }]),
		);
		renderer.root.add(response.root);
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Checking the dependency graph");

		response.updateContent(
			assistant(
				[
					{ type: "thinking", thinking: "Checking the dependency graph" },
					{ type: "text", text: "The dependency graph is valid." },
				],
				"stop",
			),
		);
		const captured = await flushUntil(setup, "The dependency graph is valid.");
		expect(captured).toContain("The dependency graph is valid.");
		expect(captured).not.toContain("Checking the dependency graph");
	});

	test("renders Responses commentary as normal Agent prose", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 80, height: 12 });
		renderers.add(setup);
		const { renderer } = setup;
		const response = new OpenTUIAssistantMessage(
			renderer,
			assistant([
				{
					type: "text",
					text: "Inspecting the event queue",
					textSignature: JSON.stringify({ v: 1, id: "commentary-1", phase: "commentary" }),
				},
				{
					type: "text",
					text: "The event queue is healthy.",
					textSignature: JSON.stringify({ v: 1, id: "answer-1", phase: "final_answer" }),
				},
			]),
		);
		renderer.root.add(response.root);

		const captured = await flushUntil(setup, "The event queue is healthy.");
		expect(captured).toContain("The event queue is healthy.");
		expect(captured).toContain("Inspecting the event queue");
		expect(
			response.root
				.getChildren()
				.slice(1)
				.every((child) => child instanceof MarkdownRenderable),
		).toBe(true);
	});

	test("renders strong and emphasized markdown with terminal attributes", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 80, height: 12 });
		renderers.add(setup);
		const response = new OpenTUIAssistantMessage(
			setup.renderer,
			assistant([{ type: "text", text: "Use the **production** configuration *carefully*." }], "stop"),
		);
		setup.renderer.root.add(response.root);

		await setup.flush();
		const production = setup
			.captureSpans()
			.lines.flatMap((line) => line.spans)
			.find((span) => span.text === "production");
		expect(production?.attributes & TextAttributes.BOLD).toBe(TextAttributes.BOLD);
		const carefully = setup
			.captureSpans()
			.lines.flatMap((line) => line.spans)
			.find((span) => span.text === "carefully");
		expect(carefully?.attributes & TextAttributes.ITALIC).toBe(TextAttributes.ITALIC);
	});

	test("renders fenced code as a bounded block without excess blank lines", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 80, height: 16 });
		renderers.add(setup);
		const response = new OpenTUIAssistantMessage(
			setup.renderer,
			assistant(
				[
					{
						type: "text",
						text: "Before the code.\n\n```ts\nconst value = 1;\n```\n\nAfter the code.",
					},
				],
				"stop",
			),
		);
		setup.renderer.root.add(response.root);

		await setup.flush();
		const rows = setup
			.captureCharFrame()
			.split("\n")
			.map((line) => line.trimEnd());
		const before = rows.findIndex((line) => line.includes("Before the code."));
		const code = rows.findIndex((line) => line.includes("const value = 1;"));
		const after = rows.findIndex((line) => line.includes("After the code."));
		expect(before).toBeGreaterThanOrEqual(0);
		expect(code - before).toBe(2);
		expect(after - code).toBe(2);
		expect(rows[code]).toContain("│");
		await settleCodeHighlighting(response.root);
	});

	test("keeps adjacent assistant text blocks on the same line during streaming updates", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 80, height: 12 });
		renderers.add(setup);
		const response = new OpenTUIAssistantMessage(
			setup.renderer,
			assistant([{ type: "text", text: "The dependency " }]),
		);
		setup.renderer.root.add(response.root);
		response.updateContent(
			assistant([
				{ type: "text", text: "The dependency " },
				{ type: "text", text: "graph is valid." },
			]),
		);

		const captured = await flushUntil(setup, "The dependency graph is valid.");
		expect(captured).toContain("The dependency graph is valid.");
	});

	test("renders a markdown table before the assistant stream finishes", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 60, height: 20 });
		renderers.add(setup);
		const lines = [
			"结果：",
			"",
			"| 项目 | 状态 | 链接或结果 |",
			"|---|---|---|",
			"| 发布版本 | 已完成 | `v1.2.3` |",
			"| GitHub Release | 已创建 | [查看 Release](https://example.com/releases/v1.2.3) |",
			"| OpenTUI 专项测试 | 通过 | `37/37` |",
			"| 完整检查 | 通过 | `bun run check` |",
		];
		const complete = lines.join("\n");
		const response = new OpenTUIAssistantMessage(setup.renderer, assistant([{ type: "text", text: "Results:" }]));
		setup.renderer.root.add(response.root);
		for (let lineCount = 1; lineCount <= lines.length; lineCount++) {
			response.updateContent(assistant([{ type: "text", text: lines.slice(0, lineCount).join("\n") }]));
			await setup.flush();
		}

		let descendants: Renderable[] = [response.root];
		for (let index = 0; index < descendants.length; index++) {
			descendants.push(...descendants[index]!.getChildren());
		}
		expect(descendants.some((node) => node instanceof TextTableRenderable)).toBe(true);
		const streamingFrame = setup.captureCharFrame();
		expect(streamingFrame).toContain("项目");
		expect(streamingFrame).toContain("完整检查");
		const streamingMarkdown = response.root.getChildren().find((node) => node instanceof MarkdownRenderable);
		expect(streamingMarkdown).toBeDefined();

		response.updateContent(assistant([{ type: "text", text: complete }], "stop"));
		await setup.flush();
		const finalFrame = setup.captureCharFrame();
		const finalMarkdown = response.root.getChildren().find((node) => node instanceof MarkdownRenderable);
		expect(finalMarkdown).toBeDefined();
		expect(finalMarkdown).not.toBe(streamingMarkdown);
		descendants = [response.root];
		for (let index = 0; index < descendants.length; index++) {
			descendants.push(...descendants[index]!.getChildren());
		}
		expect(descendants.some((node) => node instanceof TextTableRenderable)).toBe(true);

		const replaySetup = await createTestRenderer({ width: 60, height: 20 });
		renderers.add(replaySetup);
		const replay = new OpenTUIAssistantMessage(
			replaySetup.renderer,
			assistant([{ type: "text", text: complete }], "stop"),
		);
		replaySetup.renderer.root.add(replay.root);
		await replaySetup.flush();
		expect(finalFrame).toBe(replaySetup.captureCharFrame());
	});

	test("keeps markdown visible when a streamed message becomes final", async () => {
		initTheme("dark");
		const setup = await createTestRenderer({ width: 80, height: 12 });
		renderers.add(setup);
		const response = new OpenTUIAssistantMessage(
			setup.renderer,
			assistant([{ type: "text", text: "Checking **production**" }]),
		);
		setup.renderer.root.add(response.root);
		await setup.flush();

		response.updateContent(assistant([{ type: "text", text: "Checked **production** successfully." }], "stop"));
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Checked production successfully.");
	});
});
