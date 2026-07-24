import { BoxRenderable } from "@opentui/core";
import { createTestRenderer, MouseButtons, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test } from "vitest";
import {
	OpenTUIBashExecution,
	OpenTUIBranchSummary,
	OpenTUICompactionSummary,
	OpenTUICustomMessage,
	OpenTUISkillInvocation,
	OpenTUIStatusView,
	OpenTUIToolExecution,
	OpenTUIWorkingGroup,
	textOnlyToolResult,
} from "../src/modes/interactive/components/opentui-rich-messages.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<TestRendererSetup>();

async function frame(setup: TestRendererSetup, expected: string): Promise<string> {
	for (let attempt = 0; attempt < 8; attempt++) {
		await setup.flush();
		const captured = setup.captureCharFrame();
		if (captured.includes(expected)) return captured;
	}
	return setup.captureCharFrame();
}

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

async function setup() {
	initTheme("dark");
	const setup = await createTestRenderer({ width: 84, height: 32 });
	renderers.add(setup);
	return setup;
}

describe("OpenTUI rich messages", () => {
	test("updates tool and bash streaming content in place with error and expansion states", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const root = new BoxRenderable(renderer, { flexDirection: "column" });
		renderer.root.add(root);
		const tool = new OpenTUIToolExecution(renderer, "read", "call-1", { path: "README.md" });
		root.add(tool.root);
		tool.markExecutionStarted();
		let captured = await frame(testRenderer, "read · running");
		expect(captured).not.toContain("README.md");
		await testRenderer.mockMouse.pressDown(2, 2);
		expect(await frame(testRenderer, "read · running")).not.toContain("README.md");
		await testRenderer.mockMouse.release(2, 2);
		captured = await frame(testRenderer, "README.md");
		expect(captured).toContain("README.md");
		tool.setExpanded(false);
		await testRenderer.mockMouse.click(2, 2, MouseButtons.RIGHT);
		expect(await frame(testRenderer, "read · running")).not.toContain("README.md");
		await testRenderer.mockMouse.drag(2, 2, 8, 2);
		expect(await frame(testRenderer, "read · running")).not.toContain("README.md");
		await testRenderer.mockMouse.click(2, 2);
		expect(await frame(testRenderer, "README.md")).toContain("README.md");

		const manyLines = Array.from({ length: 24 }, (_, index) => `line ${index + 1}`).join("\n");
		tool.updateResult(textOnlyToolResult("read", "call-1", manyLines), true);
		captured = await frame(testRenderer, "4 earlier lines hidden");
		expect(captured).toContain("read · streaming");
		tool.setExpanded(true);
		captured = await frame(testRenderer, "line 1");
		expect(captured).not.toContain("earlier lines hidden");
		tool.updateResult(textOnlyToolResult("read", "call-1", "permission denied", true));
		expect(await frame(testRenderer, "permission denied")).toContain("read · failed");

		const bash = new OpenTUIBashExecution(renderer, "bun test");
		root.add(bash.root);
		bash.appendOutput("first\r\n");
		bash.appendOutput("\u001b[31msecond\u001b[0m");
		expect(await frame(testRenderer, "second")).toContain("Running...");
		bash.setComplete(2, false);
		expect(await frame(testRenderer, "Exited with code 2")).toContain("$ bun test");
	});

	test("renders status, summaries, skill, and custom messages as structured nodes", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const root = new BoxRenderable(renderer, { flexDirection: "column" });
		renderer.root.add(root);
		const status = new OpenTUIStatusView(renderer, "retry", "Retrying in 3s");
		root.add(status.root);
		status.tick();
		expect(await frame(testRenderer, "Retrying in 3s")).toContain("Retrying in 3s");

		const compaction = new OpenTUICompactionSummary(renderer, {
			role: "compactionSummary",
			summary: "Kept the important decisions.",
			tokensBefore: 12000,
			timestamp: 1,
		});
		root.add(compaction.root);
		expect(await frame(testRenderer, "Compacted from 12,000 tokens")).not.toContain("important decisions");
		compaction.setExpanded(true);
		expect(await frame(testRenderer, "important decisions")).toContain("[compaction]");

		const branch = new OpenTUIBranchSummary(renderer, {
			role: "branchSummary",
			summary: "Alternative path",
			fromId: "a",
			timestamp: 1,
		});
		root.add(branch.root);
		branch.setExpanded(true);
		expect(await frame(testRenderer, "Alternative path")).toContain("[branch]");

		const skill = new OpenTUISkillInvocation(renderer, {
			name: "release",
			location: "/tmp/release",
			content: "Run checks",
			userMessage: undefined,
		});
		root.add(skill.root);
		skill.setExpanded(true);
		expect(await frame(testRenderer, "Run checks")).toContain("[skill]");

		const custom = new OpenTUICustomMessage(renderer, {
			role: "custom",
			customType: "notice",
			content: "Deployment ready",
			display: true,
			timestamp: 1,
		});
		root.add(custom.root);
		expect(await frame(testRenderer, "Deployment ready")).toContain("[notice]");
	});

	test("renders unified tool output with the native diff node", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const tool = new OpenTUIToolExecution(renderer, "edit", "call-diff", { path: "a.ts" });
		renderer.root.add(tool.root);
		tool.setExpanded(true);
		tool.updateResult(
			textOnlyToolResult("edit", "call-diff", "--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n-old value\n+new value"),
		);
		const captured = await frame(testRenderer, "new value");
		expect(captured).toContain("old value");
		expect(captured).toContain("edit · complete");
	});

	test("summarizes a successful working group and toggles that group by mouse", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		let now = 0;
		const group = new OpenTUIWorkingGroup(renderer, 0, () => now);
		let firstTool: OpenTUIToolExecution | undefined;
		for (let index = 0; index < 7; index++) {
			const id = `call-${index}`;
			const tool = new OpenTUIToolExecution(renderer, "read", id, { path: `${index}.txt` });
			firstTool ??= tool;
			tool.markExecutionStarted();
			tool.updateResult(textOnlyToolResult("read", id, `result ${index}`));
			group.addTool(id, tool);
			if (index === 6) now = 18_000;
			group.markToolComplete(id, false);
		}
		renderer.root.add(group.root);
		let captured = await frame(testRenderer, "✓ Inspected the workspace · 18s · 7 tool calls");
		expect(captured).not.toContain("result 0");

		await testRenderer.mockMouse.pressDown(2, 1);
		expect(await frame(testRenderer, "Inspected the workspace")).not.toContain("read · complete");
		await testRenderer.mockMouse.release(2, 1);
		captured = await frame(testRenderer, "read · complete");
		expect(captured).toContain("⌄ ✓ Inspected the workspace · 18s · 7 tool calls");
		expect(captured).not.toContain("result 0");
		firstTool?.setExpanded(true);
		captured = await frame(testRenderer, "result 0");
		expect(captured).toContain("result 0");
	});

	test("keeps a failed working group expanded", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const group = new OpenTUIWorkingGroup(renderer, 0, () => 2_000);
		const tool = new OpenTUIToolExecution(renderer, "write", "failed-call", { path: "locked.txt" });
		tool.markExecutionStarted();
		tool.updateResult(textOnlyToolResult("write", "failed-call", "permission denied", true));
		group.addTool("failed-call", tool);
		group.markToolComplete("failed-call", true);
		renderer.root.add(group.root);

		const captured = await frame(testRenderer, "permission denied");
		expect(captured).toContain("✗ Update failed · 2s · 1 tool call");
		expect(captured).toContain("write · failed");
	});

	test("shows a failed Agent activity even when no tool failed", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const group = new OpenTUIWorkingGroup(renderer, 0, () => 2_000);
		group.waitForAgentEnd();
		group.finish(true);
		renderer.root.add(group.root);

		const captured = await frame(testRenderer, "Work failed");
		expect(captured).toContain("✗ Work failed · 2s");
		expect(captured).not.toContain("✓");
	});

	test("describes mixed file activity while preserving completion expansion rules", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const group = new OpenTUIWorkingGroup(renderer, 0, () => 3_000);
		const read = new OpenTUIToolExecution(renderer, "read", "read-call", { path: "a.ts" });
		const edit = new OpenTUIToolExecution(renderer, "apply_patch", "edit-call", { path: "a.ts" });
		group.addTool("read-call", read);
		group.addTool("edit-call", edit);
		renderer.root.add(group.root);

		let captured = await frame(testRenderer, "Inspecting and updating files · 2 tool calls");
		expect(captured).toContain("› ◐");

		read.updateResult(textOnlyToolResult("read", "read-call", "old"));
		edit.updateResult(textOnlyToolResult("apply_patch", "edit-call", "done"));
		group.markToolComplete("read-call", false);
		group.markToolComplete("edit-call", false);
		captured = await frame(testRenderer, "Inspected and updated files · 3s · 2 tool calls");
		expect(captured).toContain("› ✓");
		expect(captured).not.toContain("old");
	});
});
