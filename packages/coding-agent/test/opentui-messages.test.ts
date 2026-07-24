import type { AssistantMessage } from "@frelion/bone-ai";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test } from "vitest";
import { OpenTUIAssistantMessage, OpenTUIUserMessage } from "../src/modes/interactive/components/opentui-messages.ts";
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
});
