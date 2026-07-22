import type { AssistantMessage } from "@frelion/bone-ai";
import { createBoneTestRenderer } from "@frelion/bone-tui";
import { afterEach, describe, expect, test } from "vitest";
import { OpenTUIAssistantMessage, OpenTUIUserMessage } from "../src/modes/interactive/components/opentui-messages.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<Awaited<ReturnType<typeof createBoneTestRenderer>>>();

afterEach(() => {
	for (const renderer of renderers) renderer.destroy();
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
	test("renders Codex-style prompts and a label-free assistant stream", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 80, height: 16 });
		renderers.add(renderer);
		renderer.start();
		renderer.content.append(new OpenTUIUserMessage("inspect this repository").mount(renderer));
		const response = new OpenTUIAssistantMessage(assistant([{ type: "text", text: "Reading the files" }]));
		renderer.content.append(response.mount(renderer));
		await renderer.flush();
		const captured = renderer.captureFrame();
		expect(captured).toContain("› inspect this repository");
		expect(captured).toContain("Reading the files");
		expect(captured).not.toContain("YOU");
		expect(captured).not.toContain("BONE");
	});

	test("shows thinking only while the assistant is running", async () => {
		initTheme("dark");
		const renderer = await createBoneTestRenderer({ width: 80, height: 12 });
		renderers.add(renderer);
		renderer.start();
		const response = new OpenTUIAssistantMessage(
			assistant([{ type: "thinking", thinking: "Checking the dependency graph" }]),
		);
		renderer.content.append(response.mount(renderer));
		await renderer.flush();
		expect(renderer.captureFrame()).toContain("Checking the dependency graph");

		response.updateContent(
			assistant(
				[
					{ type: "thinking", thinking: "Checking the dependency graph" },
					{ type: "text", text: "The dependency graph is valid." },
				],
				"stop",
			),
		);
		await renderer.flush();
		const captured = renderer.captureFrame();
		expect(captured).toContain("The dependency graph is valid.");
		expect(captured).not.toContain("Checking the dependency graph");
	});
});
