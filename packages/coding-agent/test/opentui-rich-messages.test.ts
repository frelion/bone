import { createBoneTestRenderer } from "@frelion/bone-tui";
import { afterEach, describe, expect, test } from "vitest";
import {
	OpenTUIBashExecution,
	OpenTUIBranchSummary,
	OpenTUICompactionSummary,
	OpenTUICustomMessage,
	OpenTUISkillInvocation,
	OpenTUIStatusView,
	OpenTUIToolExecution,
	textOnlyToolResult,
} from "../src/modes/interactive/components/opentui-rich-messages.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<Awaited<ReturnType<typeof createBoneTestRenderer>>>();

async function frame(renderer: Awaited<ReturnType<typeof createBoneTestRenderer>>, expected: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await renderer.flush();
		const captured = renderer.captureFrame();
		if (captured.includes(expected)) return captured;
	}
	return renderer.captureFrame();
}

afterEach(() => {
	for (const renderer of renderers) renderer.destroy();
	renderers.clear();
});

async function setup() {
	initTheme("dark");
	const renderer = await createBoneTestRenderer({ width: 84, height: 32 });
	renderers.add(renderer);
	renderer.start();
	return renderer;
}

describe("OpenTUI rich messages", () => {
	test("updates tool and bash streaming content in place with error and expansion states", async () => {
		const renderer = await setup();
		const root = renderer.createBox({ flexDirection: "column" });
		renderer.content.append(root);
		const tool = new OpenTUIToolExecution("read", "call-1", { path: "README.md" });
		root.append(tool.mount(renderer));
		tool.markExecutionStarted();
		expect(await frame(renderer, "read · running")).toContain("README.md");

		const manyLines = Array.from({ length: 24 }, (_, index) => `line ${index + 1}`).join("\n");
		tool.updateResult(textOnlyToolResult("read", "call-1", manyLines), true);
		let captured = await frame(renderer, "4 earlier lines hidden");
		expect(captured).toContain("read · streaming");
		tool.setExpanded(true);
		captured = await frame(renderer, "line 1");
		expect(captured).not.toContain("earlier lines hidden");
		tool.updateResult(textOnlyToolResult("read", "call-1", "permission denied", true));
		expect(await frame(renderer, "permission denied")).toContain("read · failed");

		const bash = new OpenTUIBashExecution("bun test");
		root.append(bash.mount(renderer));
		bash.appendOutput("first\r\n");
		bash.appendOutput("\u001b[31msecond\u001b[0m");
		expect(await frame(renderer, "second")).toContain("Running...");
		bash.setComplete(2, false);
		expect(await frame(renderer, "Exited with code 2")).toContain("$ bun test");
	});

	test("renders status, summaries, skill, and custom messages as structured nodes", async () => {
		const renderer = await setup();
		const root = renderer.createBox({ flexDirection: "column" });
		renderer.content.append(root);
		const status = new OpenTUIStatusView("retry", "Retrying in 3s");
		root.append(status.mount(renderer));
		status.tick();
		expect(await frame(renderer, "Retrying in 3s")).toContain("Retrying in 3s");

		const compaction = new OpenTUICompactionSummary({
			role: "compactionSummary",
			summary: "Kept the important decisions.",
			tokensBefore: 12000,
			timestamp: 1,
		});
		root.append(compaction.mount(renderer));
		expect(await frame(renderer, "Compacted from 12,000 tokens")).not.toContain("important decisions");
		compaction.setExpanded(true);
		expect(await frame(renderer, "important decisions")).toContain("[compaction]");

		const branch = new OpenTUIBranchSummary({
			role: "branchSummary",
			summary: "Alternative path",
			fromId: "a",
			timestamp: 1,
		});
		root.append(branch.mount(renderer));
		branch.setExpanded(true);
		expect(await frame(renderer, "Alternative path")).toContain("[branch]");

		const skill = new OpenTUISkillInvocation({
			name: "release",
			location: "/tmp/release",
			content: "Run checks",
			userMessage: undefined,
		});
		root.append(skill.mount(renderer));
		skill.setExpanded(true);
		expect(await frame(renderer, "Run checks")).toContain("[skill]");

		const custom = new OpenTUICustomMessage({
			role: "custom",
			customType: "notice",
			content: "Deployment ready",
			display: true,
			timestamp: 1,
		});
		root.append(custom.mount(renderer));
		expect(await frame(renderer, "Deployment ready")).toContain("[notice]");
	});

	test("renders unified tool output with the native diff node", async () => {
		const renderer = await setup();
		const tool = new OpenTUIToolExecution("edit", "call-diff", { path: "a.ts" });
		renderer.content.append(tool.mount(renderer));
		tool.updateResult(
			textOnlyToolResult("edit", "call-diff", "--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n-old value\n+new value"),
		);
		const captured = await frame(renderer, "new value");
		expect(captured).toContain("old value");
		expect(captured).toContain("edit · complete");
	});
});
