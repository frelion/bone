import { BoxRenderable } from "@opentui/core";
import { createTestRenderer, MouseButtons, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test } from "vitest";
import {
	OpenTUIActionExecution,
	OpenTUIBashExecution,
	OpenTUIBranchSummary,
	OpenTUICompactionSummary,
	OpenTUICustomMessage,
	OpenTUISkillInvocation,
	OpenTUIStatusView,
	OpenTUIToolExecution,
	OpenTUIWorkingGroup,
	summarizeOpenTUIToolCall,
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
	test("builds bounded one-line tool summaries from the primary target", () => {
		expect(summarizeOpenTUIToolCall("bash", { command: "bun test\n--watch" }, { phase: "running" })).toBe(
			"bash · bun test --watch · running",
		);
		const long = summarizeOpenTUIToolCall(
			"read",
			{ path: `/workspace/${"deep/".repeat(30)}file.ts` },
			{ phase: "complete", result: textOnlyToolResult("read", "summary-call", "one\ntwo\nthree") },
		);
		expect(long).toContain("3 lines");
		expect(long).not.toContain("\n");
		expect(long.length).toBeLessThanOrEqual(104);
		const failed = summarizeOpenTUIToolCall(
			"write",
			{ path: `/workspace/${"deep/".repeat(30)}locked.ts` },
			{
				phase: "failed",
				result: textOnlyToolResult("write", "failed-summary", "permission denied\nlong stack", true),
			},
		);
		expect(failed).toContain("failed: permission denied");
		expect(failed.length).toBeLessThanOrEqual(104);
	});

	test("updates tool and bash streaming content in place with error and expansion states", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const root = new BoxRenderable(renderer, { flexDirection: "column" });
		renderer.root.add(root);
		const tool = new OpenTUIToolExecution(renderer, "read", "call-1", { path: "README.md" });
		root.add(tool.root);
		tool.markExecutionStarted();
		let captured = await frame(testRenderer, "read · README.md · running");
		expect(captured).not.toContain('"path": "README.md"');
		expect(tool.root.height).toBe(1);
		const toolBody = tool.root.getChildren()[0] as BoxRenderable;
		const toolTitle = toolBody.getChildren()[0];
		if (!toolTitle) throw new Error("Expected tool title");
		const restingToolTitleAttributes = toolTitle.attributes;
		await testRenderer.mockMouse.moveTo(toolTitle.screenX + 1, toolTitle.screenY);
		await testRenderer.flush();
		expect(toolTitle.attributes).not.toBe(restingToolTitleAttributes);
		await testRenderer.mockMouse.moveTo(40, 20);
		await testRenderer.flush();
		expect(toolTitle.attributes).toBe(restingToolTitleAttributes);
		await testRenderer.mockMouse.pressDown(toolTitle.screenX + 1, toolTitle.screenY);
		expect(await frame(testRenderer, "read · README.md · running")).not.toContain('"path": "README.md"');
		await testRenderer.mockMouse.release(toolTitle.screenX + 1, toolTitle.screenY);
		captured = await frame(testRenderer, "README.md");
		expect(captured).toContain("README.md");
		tool.setExpanded(false);
		await testRenderer.mockMouse.click(toolTitle.screenX + 1, toolTitle.screenY, MouseButtons.RIGHT);
		expect(await frame(testRenderer, "read · README.md · running")).not.toContain('"path": "README.md"');
		await testRenderer.mockMouse.drag(
			toolTitle.screenX + 1,
			toolTitle.screenY,
			toolTitle.screenX + 7,
			toolTitle.screenY,
		);
		expect(await frame(testRenderer, "read · README.md · running")).not.toContain('"path": "README.md"');
		await testRenderer.mockMouse.click(toolTitle.screenX + 1, toolTitle.screenY);
		expect(await frame(testRenderer, "README.md")).toContain("README.md");

		const manyLines = Array.from({ length: 24 }, (_, index) => `line ${index + 1}`).join("\n");
		tool.updateResult(textOnlyToolResult("read", "call-1", manyLines), true);
		captured = await frame(testRenderer, "line 24");
		expect(captured).toContain("read · README.md · streaming");
		expect(captured).toContain("line 1");
		expect(captured).toContain("line 20");
		expect(captured).toContain("line 21");
		expect(captured).not.toContain("lines hidden");
		expect(captured).not.toContain("Show all");
		tool.updateResult(textOnlyToolResult("read", "call-1", "permission denied", true));
		expect(await frame(testRenderer, "permission denied")).toContain("read · README.md · failed");

		const bash = new OpenTUIBashExecution(renderer, "bun test");
		root.add(bash.root);
		bash.appendOutput("first\r\n");
		bash.appendOutput("\u001b[31msecond\u001b[0m");
		expect(await frame(testRenderer, "second")).toContain("Running...");
		bash.setComplete(2, false);
		expect(await frame(testRenderer, "Exited with code 2")).toContain("$ bun test");
	});

	test("rolls the highlight across an active action and becomes static at completion", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		let now = 0;
		const action = new OpenTUIActionExecution(renderer, "action-highlight", "Inspecting README.md", {
			now: () => now,
		});
		renderer.root.add(action.root);
		await testRenderer.flush();
		const initial = JSON.stringify(testRenderer.captureSpans());

		now = 80;
		action.tickActivity();
		await testRenderer.flush();
		const advanced = JSON.stringify(testRenderer.captureSpans());
		expect(advanced).not.toBe(initial);

		action.setStatus("completed");
		await testRenderer.flush();
		const completed = JSON.stringify(testRenderer.captureSpans());
		now = 320;
		action.tickActivity();
		await testRenderer.flush();
		expect(JSON.stringify(testRenderer.captureSpans())).toBe(completed);
		expect(testRenderer.captureCharFrame()).toContain("Inspecting README.md");
	});

	test("renders sub-agent lifecycle rows and bounded handoff details inside the parent action", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const action = new OpenTUIActionExecution(renderer, "action-subagents", "Inspecting runtime");
		renderer.root.add(action.root);
		action.setSubagents([
			{
				agentRef: "agent-1",
				label: "Runtime review",
				scope: "exchange",
				status: "running",
				yields: [],
				unreadYieldCount: 0,
				origin: { exchangeId: "exchange-1", actionId: "action-subagents", toolCallId: "call-1" },
				createdAt: 1,
				lastActivityAt: 2,
			},
		]);

		let captured = await frame(testRenderer, "◐ running");
		expect(captured).toContain("Runtime review");

		action.setSubagents([
			{
				agentRef: "agent-1",
				label: "Runtime review",
				scope: "exchange",
				status: "idle",
				lastRunStatus: "completed",
				origin: { exchangeId: "exchange-1", actionId: "action-subagents", toolCallId: "call-1" },
				handoff: {
					status: "completed",
					summary: "Cancellation races are covered.",
					validations: ["20 runtime tests passed"],
				},
				yields: [],
				unreadYieldCount: 0,
				createdAt: 1,
				lastActivityAt: 3,
			},
		]);
		action.setStatus("completed");
		captured = await frame(testRenderer, "1 agent · complete");
		expect(captured).not.toContain("✓ completed");
		expect(captured).not.toContain("Cancellation races are covered.");

		action.setExpanded(true);
		await frame(testRenderer, "✓ completed");
		const subagentsRoot = action.root.getChildren()[2] as BoxRenderable;
		const childRoot = subagentsRoot.getChildren()[0] as BoxRenderable;
		const childHeader = childRoot.getChildren()[0];
		if (!childHeader) throw new Error("Expected child-agent header");
		await testRenderer.mockMouse.click(childHeader.screenX + 1, childHeader.screenY);
		captured = await frame(testRenderer, "Cancellation races are covered.");
		expect(captured).toContain("✓ completed");
		expect(captured).toContain("Validated: 20 runtime tests passed");

		action.setSubagents([
			{
				agentRef: "agent-1",
				label: "Runtime review",
				scope: "exchange",
				status: "running",
				yields: [],
				unreadYieldCount: 0,
				origin: { exchangeId: "exchange-1", actionId: "action-subagents", toolCallId: "call-1" },
				createdAt: 1,
				lastActivityAt: 4,
			},
		]);
		captured = await frame(testRenderer, "1 agent · active");
		expect(captured).not.toContain("1 agent · complete");

		action.setSubagents([
			{
				agentRef: "agent-1",
				label: "Runtime review",
				scope: "exchange",
				status: "idle",
				lastRunStatus: "completed",
				origin: { exchangeId: "exchange-1", actionId: "action-subagents", toolCallId: "call-1" },
				handoff: { status: "partial", summary: "More validation is required." },
				yields: [],
				unreadYieldCount: 0,
				createdAt: 1,
				lastActivityAt: 5,
			},
		]);
		captured = await frame(testRenderer, "1 agent · partial");
		expect(captured).toContain("△ partial");
	});

	test("renders yielded child messages as unread progressive details", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const action = new OpenTUIActionExecution(renderer, "action-yields", "Reviewing delegated findings");
		renderer.root.add(action.root);
		const yields = [
			{
				agentRef: "agent-yield",
				sequence: 1,
				kind: "finding" as const,
				message: "The parser already exposes a stable boundary.",
				artifactRefs: ["src/parser.ts"],
				createdAt: 2,
			},
			{
				agentRef: "agent-yield",
				sequence: 2,
				kind: "risk" as const,
				message: "The compatibility shim still needs validation.",
				createdAt: 3,
			},
		];
		const execution = {
			agentRef: "agent-yield",
			label: "Parser investigator",
			scope: "conversation" as const,
			status: "running" as const,
			origin: { exchangeId: "exchange-yield", actionId: "action-yields", toolCallId: "delegate-yield" },
			yields,
			unreadYieldCount: 2,
			createdAt: 1,
			lastActivityAt: 3,
		};
		action.setSubagents([execution]);

		let captured = await frame(testRenderer, "2 new");
		expect(captured).toContain("Parser investigator");
		expect(captured).not.toContain("stable boundary");
		action.setStatus("completed");
		captured = await frame(testRenderer, "1 agent · complete · 2 new messages");
		expect(captured).not.toContain("Parser investigator");
		action.setExpanded(true);
		await frame(testRenderer, "Parser investigator");
		const subagentsRoot = action.root.getChildren()[2] as BoxRenderable;
		const childRoot = subagentsRoot.getChildren()[0] as BoxRenderable;
		const childHeader = childRoot.getChildren()[0];
		if (!childHeader) throw new Error("Expected child-agent header");
		await testRenderer.mockMouse.click(childHeader.screenX + 1, childHeader.screenY);
		captured = await frame(testRenderer, "stable boundary");
		expect(captured).toContain("↑ finding #1");
		expect(captured).toContain("↑ risk #2");
		expect(captured).toContain("Refs: src/parser.ts");

		action.setSubagents([{ ...execution, unreadYieldCount: 0 }]);
		captured = await frame(testRenderer, "2 messages");
		expect(captured).not.toContain("2 new");
		expect(captured).toContain("stable boundary");
	});

	test("shows complete long command output when expanded", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const tool = new OpenTUIToolExecution(renderer, "bash", "call-command", { command: "bun test" });
		renderer.root.add(tool.root);
		tool.markExecutionStarted();
		tool.setDetailLevel("full");
		const output = Array.from({ length: 24 }, (_, index) => `command line ${index + 1}`).join("\n");
		tool.updateResult(textOnlyToolResult("bash", "call-command", output));

		const captured = await frame(testRenderer, "command line 24");
		expect(captured).toContain("command line 1");
		expect(captured).toContain("command line 24");
		expect(captured).not.toContain("Show all");
	});

	test("renders status, summaries, skill, and custom messages as structured nodes", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		const root = new BoxRenderable(renderer, { flexDirection: "column" });
		renderer.root.add(root);
		const status = new OpenTUIStatusView(renderer, "retry", "Retrying in 3s");
		root.add(status.root);
		await testRenderer.flush();
		const statusBeforeTick = JSON.stringify(testRenderer.captureSpans());
		status.tick();
		await testRenderer.flush();
		const statusAfterTick = JSON.stringify(testRenderer.captureSpans());
		expect(statusAfterTick).not.toBe(statusBeforeTick);
		expect(await frame(testRenderer, "Retrying in 3s")).not.toMatch(/◐|◓|◑|◒/);

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
		expect(captured).toContain("edit · a.ts · complete");
	});

	test("shows successful actions directly and expands one action by mouse", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		let now = 0;
		const group = new OpenTUIWorkingGroup(renderer, 0, () => now);
		let firstTool: OpenTUIToolExecution | undefined;
		let firstAction: OpenTUIActionExecution | undefined;
		for (let index = 0; index < 7; index++) {
			const id = `call-${index}`;
			const tool = new OpenTUIToolExecution(renderer, "read", id, { path: `${index}.txt` });
			firstTool ??= tool;
			tool.markExecutionStarted();
			tool.updateResult(textOnlyToolResult("read", id, `result ${index}`));
			const action = new OpenTUIActionExecution(renderer, `action-${index}`, tool.getSummaryText());
			action.addTool(id, tool);
			firstAction ??= action;
			group.addTool(`action-${index}`, action);
			if (index === 6) now = 18_000;
			group.markToolComplete(`action-${index}`, false);
		}
		renderer.root.add(group.root);
		let captured = await frame(testRenderer, "read · 0.txt · complete");
		expect(captured).toContain("read · 6.txt · complete");
		expect(captured).not.toContain("result 0");
		const firstActionTitle = firstAction?.getSummaryNode();
		const firstToolTitle = firstTool?.getSummaryNode();
		if (!firstActionTitle || !firstToolTitle) throw new Error("Expected first action and tool titles");
		await testRenderer.mockMouse.click(firstActionTitle.screenX + 1, firstActionTitle.screenY);
		captured = await frame(testRenderer, "read · 0.txt · complete");
		expect(captured).not.toContain("result 0");
		await testRenderer.mockMouse.click(firstToolTitle.screenX + 1, firstToolTitle.screenY);
		captured = await frame(testRenderer, "result 0");
		expect(captured).toContain("result 0");
		expect(captured).not.toContain("Working");
	});

	test("keeps a failed action collapsed until action and ToolCall are opened in sequence", async () => {
		initTheme("dark");
		const testRenderer = await createTestRenderer({ width: 84, height: 64 });
		renderers.add(testRenderer);
		const { renderer } = testRenderer;
		const group = new OpenTUIWorkingGroup(renderer, 0, () => 2_000);
		const tool = new OpenTUIToolExecution(renderer, "write", "failed-call", { path: "locked.txt" });
		tool.markExecutionStarted();
		const longError = [
			"permission denied",
			...Array.from({ length: 40 }, (_, index) => `stack line ${index + 1}`),
		].join("\n");
		tool.updateResult(textOnlyToolResult("write", "failed-call", longError, true));
		const action = new OpenTUIActionExecution(renderer, "failed-action", "Updating locked.txt");
		action.addTool("failed-call", tool);
		group.addTool("failed-action", action);
		group.markToolComplete("failed-action", true);
		renderer.root.add(group.root);

		let captured = await frame(testRenderer, "Updating locked.txt");
		expect(captured).not.toContain("permission denied");
		const actionTitle = action.getSummaryNode();
		await testRenderer.mockMouse.click(actionTitle.screenX + 1, actionTitle.screenY);
		captured = await frame(testRenderer, "permission denied");
		expect(captured).toContain("write · locked.txt · failed: permission denied");
		expect(captured).not.toContain('"path": "locked.txt"');
		const toolTitle = tool.getSummaryNode();
		await testRenderer.mockMouse.click(toolTitle.screenX + 1, toolTitle.screenY);
		captured = await frame(testRenderer, "stack line 40");
		expect(captured).toContain('"path": "locked.txt"');
		expect(tool.getDetailLevel()).toBe("full");
	});

	test("keeps a failed ToolCall collapsed until its summary is clicked", async () => {
		const testRenderer = await setup();
		const { renderer } = testRenderer;
		let controlledLevel: "collapsed" | "full" = "collapsed";
		let tool: OpenTUIToolExecution;
		tool = new OpenTUIToolExecution(
			renderer,
			"forge_issue",
			"extension-failure",
			{ action: "create" },
			{
				onDetailLevelChange: (level) => {
					controlledLevel = level;
					tool.setDetailLevel(level);
				},
				summarize: ({ phase }) => `Create issue · ${phase}`,
			},
		);
		renderer.root.add(tool.root);
		tool.markExecutionStarted();
		tool.updateResult(textOnlyToolResult("forge_issue", "extension-failure", "API rejected request", true));
		await testRenderer.flush();
		expect(controlledLevel).toBe("collapsed");
		expect(tool.getDetailLevel()).toBe("collapsed");

		const title = tool.getSummaryNode();
		await testRenderer.mockMouse.click(title.screenX + 1, title.screenY);
		expect(controlledLevel).toBe("full");
		expect(tool.getDetailLevel()).toBe("full");
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
		const readAction = new OpenTUIActionExecution(renderer, "read-action", read.getSummaryText());
		const editAction = new OpenTUIActionExecution(renderer, "edit-action", edit.getSummaryText());
		readAction.addTool("read-call", read);
		editAction.addTool("edit-call", edit);
		group.addTool("read-action", readAction);
		group.addTool("edit-action", editAction);
		renderer.root.add(group.root);

		let captured = await frame(testRenderer, "apply_patch · a.ts · preparing");
		expect(captured).toContain("read · a.ts · preparing");
		expect(captured).not.toContain("Working");

		read.updateResult(textOnlyToolResult("read", "read-call", "old"));
		edit.updateResult(textOnlyToolResult("apply_patch", "edit-call", "done"));
		readAction.setTitle(read.getSummaryText());
		editAction.setTitle(edit.getSummaryText());
		group.markToolComplete("read-action", false);
		group.markToolComplete("edit-action", false);
		captured = await frame(testRenderer, "apply_patch · a.ts · complete");
		expect(captured).toContain("read · a.ts · complete");
		expect(captured).not.toContain("old");
	});
});
