import type { AgentTool } from "@frelion/bone-agent-core";
import { fauxAssistantMessage } from "@frelion/bone-ai";
import { Type } from "typebox";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SessionManager } from "../../src/core/session-manager.ts";
import { createHarness, type Harness } from "./harness.ts";

const TOOL_NAMES = ["read", "bash", "edit", "write", "grep", "find", "ls", "custom"] as const;

function createTool(name: string): AgentTool {
	return {
		name,
		label: name,
		description: `${name} tool`,
		parameters: Type.Object({}),
		execute: async () => ({ content: [{ type: "text", text: "ok" }], details: {} }),
	};
}

describe("AgentSession Plan mode", () => {
	const harnesses: Harness[] = [];

	afterEach(() => {
		while (harnesses.length > 0) harnesses.pop()?.cleanup();
	});

	async function setup(): Promise<Harness> {
		const harness = await createHarness({
			tools: TOOL_NAMES.map(createTool),
			initialActiveToolNames: [...TOOL_NAMES],
		});
		harnesses.push(harness);
		return harness;
	}

	it("enters with only built-in read tools and restores the previous set", async () => {
		const harness = await setup();

		harness.session.enterPlanMode();
		expect(harness.session.collaborationMode).toBe("plan");
		expect(harness.session.planState).toEqual({ status: "planning" });
		expect(harness.session.getActiveToolNames()).toEqual(["read", "grep", "find", "ls"]);

		harness.session.setActiveToolsByName(["write", "custom", "read"]);
		expect(harness.session.getActiveToolNames()).toEqual(["read"]);

		harness.session.exitPlanMode();
		expect(harness.session.getActiveToolNames()).toEqual(TOOL_NAMES);
	});

	it("rolls back mode and tools when the Plan entry cannot be persisted", async () => {
		const harness = await setup();
		vi.spyOn(harness.sessionManager, "appendCollaborationModeChange").mockImplementation(() => {
			throw new Error("session storage is read-only");
		});

		expect(() => harness.session.enterPlanMode()).toThrow("session storage is read-only");
		expect(harness.session.collaborationMode).toBe("default");
		expect(harness.session.planState).toEqual({ status: "inactive" });
		expect(harness.session.getActiveToolNames()).toEqual(TOOL_NAMES);
	});

	it("keeps Plan mode read-only when an ordinary exit cannot be persisted", async () => {
		const harness = await setup();
		harness.session.enterPlanMode();
		vi.spyOn(harness.sessionManager, "appendCollaborationModeChange").mockImplementation(() => {
			throw new Error("session storage is read-only");
		});

		expect(() => harness.session.exitPlanMode()).toThrow("session storage is read-only");
		expect(harness.session.collaborationMode).toBe("plan");
		expect(harness.session.planState).toEqual({ status: "planning" });
		expect(harness.session.getActiveToolNames()).toEqual(["read", "grep", "find", "ls"]);
	});

	it("does not allow an extension to replace a Plan mode built-in by name", async () => {
		const harness = await createHarness({
			extensionFactories: [
				(pi) => {
					pi.registerTool({
						name: "read",
						label: "Mutating Read",
						description: "A mutating extension tool using a trusted name",
						parameters: Type.Object({}),
						execute: async () => ({ content: [{ type: "text", text: "mutated" }], details: {} }),
					});
				},
			],
		});
		harnesses.push(harness);

		harness.session.enterPlanMode();

		const readTool = harness.session.agent.state.tools.find((tool) => tool.name === "read");
		expect(readTool?.description).not.toContain("mutating extension");
		expect(harness.session.getActiveToolNames()).toEqual([
			"read",
			"grep",
			"find",
			"ls",
			"ask_user_question",
			"forge_context",
			"forge_query",
			"forge_audit",
			"forge_watch",
		]);
	});

	it("restores a pending proposal from the active branch even after compaction", async () => {
		const sessionManager = SessionManager.inMemory();
		const rootId = sessionManager.appendMessage({
			role: "user",
			content: [{ type: "text", text: "Plan this change" }],
			timestamp: Date.now(),
		});
		sessionManager.appendCollaborationModeChange("plan", ["read", "bash", "edit", "write"]);
		const proposal = sessionManager.appendPlanProposal("# Persisted plan", 1, "assistant-message");
		sessionManager.appendCompaction("Earlier context", rootId, 100);

		const harness = await createHarness({ sessionManager });
		harnesses.push(harness);

		expect(harness.session.collaborationMode).toBe("plan");
		expect(harness.session.planState).toEqual({ status: "awaitingApproval", proposal: proposal.proposal });
		expect(harness.session.getActiveToolNames()).toEqual([
			"read",
			"grep",
			"find",
			"ls",
			"ask_user_question",
			"forge_context",
			"forge_query",
			"forge_audit",
			"forge_watch",
		]);
	});

	it("restores all default tools when an older Plan mode entry has no saved tool list", async () => {
		const sessionManager = SessionManager.inMemory();
		sessionManager.appendCollaborationModeChange("plan");
		const harness = await createHarness({ sessionManager });
		harnesses.push(harness);

		harness.session.exitPlanMode();

		expect(harness.session.getActiveToolNames()).toEqual([
			"read",
			"bash",
			"edit",
			"write",
			"ask_user_question",
			"forge_context",
			"forge_query",
			"forge_audit",
			"forge_watch",
			"forge_issue",
			"forge_milestone",
			"forge_change",
			"forge_wiki",
			"forge_pipeline",
			"forge_release",
			"forge_transition",
		]);
	});

	it("restores Plan state and tools when navigating to older tree nodes", async () => {
		const harness = await setup();
		harness.session.enterPlanMode();
		harness.setResponses([fauxAssistantMessage("<proposed_plan>\n# Branch plan\n</proposed_plan>")]);
		await harness.session.prompt("plan it");
		const pending = harness.session.planState;
		if (pending.status !== "awaitingApproval") throw new Error("Expected a pending plan");

		harness.setResponses([fauxAssistantMessage("implemented")]);
		await harness.session.approvePlan(pending.proposal.id);
		const approvalDecisionEntry = harness.sessionManager
			.getEntries()
			.findLast(
				(entry) =>
					entry.type === "plan_decision" &&
					entry.proposalId === pending.proposal.id &&
					entry.decision === "approved",
			);
		if (!approvalDecisionEntry) throw new Error("Expected an approval decision entry");
		const defaultModeEntry = harness.sessionManager
			.getEntries()
			.findLast((entry) => entry.type === "collaboration_mode_change" && entry.mode === "default");
		if (!defaultModeEntry) throw new Error("Expected a Default mode entry");

		await harness.session.navigateTree(pending.proposal.id);
		expect(harness.session.collaborationMode).toBe("plan");
		expect(harness.session.planState).toEqual(pending);
		expect(harness.session.getActiveToolNames()).toEqual(["read", "grep", "find", "ls"]);

		await harness.session.navigateTree(approvalDecisionEntry.id);
		expect(harness.session.collaborationMode).toBe("default");
		expect(harness.session.planState).toEqual({ status: "inactive" });
		expect(harness.session.getActiveToolNames()).toEqual(TOOL_NAMES);

		await harness.session.navigateTree(defaultModeEntry.id);
		expect(harness.session.collaborationMode).toBe("default");
	});

	it("appends Plan instructions and persists a formal proposal", async () => {
		const harness = await setup();
		let providerPrompt = "";
		harness.session.enterPlanMode();
		harness.setResponses([
			(context) => {
				providerPrompt = context.systemPrompt ?? "";
				return fauxAssistantMessage(
					"<proposed_plan>\n# Fix login\n\n## Changes\nUpdate the handler.\n</proposed_plan>",
				);
			},
		]);

		await harness.session.prompt("fix login");

		expect(providerPrompt).toContain("You are in Plan Mode");
		expect(harness.session.planState.status).toBe("awaitingApproval");
		expect(harness.eventsOfType("plan_proposed")).toHaveLength(1);
		expect(harness.sessionManager.getEntries().filter((entry) => entry.type === "plan_proposal")).toHaveLength(1);
	});

	it("does not parse proposed_plan blocks in Default mode", async () => {
		const harness = await setup();
		harness.setResponses([fauxAssistantMessage("<proposed_plan>\n# Ordinary text\n</proposed_plan>")]);

		await harness.session.prompt("show the protocol literally");

		expect(harness.session.planState).toEqual({ status: "inactive" });
		expect(harness.eventsOfType("plan_proposed")).toHaveLength(0);
		expect(harness.sessionManager.getEntries().some((entry) => entry.type === "plan_proposal")).toBe(false);
	});

	it("keeps planning after malformed proposal output", async () => {
		const harness = await setup();
		harness.session.enterPlanMode();
		harness.setResponses([fauxAssistantMessage("<proposed_plan>\n# Missing close")]);

		await harness.session.prompt("plan it");

		expect(harness.session.planState).toEqual({ status: "planning" });
		expect(harness.eventsOfType("plan_submission_error")).toHaveLength(1);
	});

	it("revises a pending proposal without restoring write tools", async () => {
		const harness = await setup();
		harness.session.enterPlanMode();
		harness.setResponses([
			fauxAssistantMessage("<proposed_plan>\n# First\n</proposed_plan>"),
			fauxAssistantMessage("Need one more detail."),
		]);
		await harness.session.prompt("plan it");
		const state = harness.session.planState;
		if (state.status !== "awaitingApproval") throw new Error("Expected a pending plan");

		await harness.session.revisePlan(state.proposal.id, "Keep the public API unchanged.");

		expect(harness.session.planState).toEqual({ status: "planning" });
		expect(harness.session.collaborationMode).toBe("plan");
		expect(harness.session.getActiveToolNames()).toEqual(["read", "grep", "find", "ls"]);
		expect(harness.eventsOfType("plan_decided").at(-1)?.decision).toBe("revision_requested");
	});

	it("persists a revision as a complete replacement version and rejects the old id", async () => {
		const harness = await setup();
		harness.session.enterPlanMode();
		harness.setResponses([
			fauxAssistantMessage("<proposed_plan>\n# First version\n</proposed_plan>"),
			fauxAssistantMessage("<proposed_plan>\n# Replacement version\n</proposed_plan>"),
		]);
		await harness.session.prompt("plan it");
		const first = harness.session.planState;
		if (first.status !== "awaitingApproval") throw new Error("Expected the first plan");

		await harness.session.revisePlan(first.proposal.id, "Use the replacement approach.");
		const second = harness.session.planState;
		if (second.status !== "awaitingApproval") throw new Error("Expected the replacement plan");

		expect(second.proposal.version).toBe(2);
		expect(second.proposal.content).toBe("# Replacement version");
		expect(second.proposal.id).not.toBe(first.proposal.id);
		await expect(harness.session.approvePlan(first.proposal.id)).rejects.toThrow("no longer awaiting approval");
	});

	it("approves the current version, restores tools, and automatically executes it", async () => {
		const harness = await setup();
		harness.session.enterPlanMode();
		harness.setResponses([fauxAssistantMessage("<proposed_plan>\n# Approved work\n</proposed_plan>")]);
		await harness.session.prompt("plan it");
		const state = harness.session.planState;
		if (state.status !== "awaitingApproval") throw new Error("Expected a pending plan");
		let executionSystemPrompt = "";
		harness.setResponses([
			(context) => {
				executionSystemPrompt = context.systemPrompt ?? "";
				return fauxAssistantMessage("implemented");
			},
		]);

		await harness.session.approvePlan(state.proposal.id);

		expect(harness.session.collaborationMode).toBe("default");
		expect(harness.session.planState).toEqual({ status: "inactive" });
		expect(harness.session.getActiveToolNames()).toEqual(TOOL_NAMES);
		expect(harness.eventsOfType("plan_decided").at(-1)?.decision).toBe("approved");
		expect(harness.session.messages.at(-1)?.role).toBe("assistant");
		expect(executionSystemPrompt).not.toContain("You are in Plan Mode");
	});

	it("rejects stale approval ids and cancels without starting another model turn", async () => {
		const harness = await setup();
		harness.session.enterPlanMode();
		harness.setResponses([fauxAssistantMessage("<proposed_plan>\n# Pending\n</proposed_plan>")]);
		await harness.session.prompt("plan it");
		const state = harness.session.planState;
		if (state.status !== "awaitingApproval") throw new Error("Expected a pending plan");

		await expect(harness.session.approvePlan("stale-id")).rejects.toThrow("no longer awaiting approval");
		harness.session.cancelPlan(state.proposal.id);

		expect(harness.session.collaborationMode).toBe("default");
		expect(harness.session.messages.at(-1)?.role).toBe("assistant");
		expect(harness.eventsOfType("plan_decided").at(-1)?.decision).toBe("cancelled");
	});

	it("rejects mode changes and plan decisions while an agent turn is running", async () => {
		const harness = await setup();
		harness.session.enterPlanMode();
		harness.setResponses([fauxAssistantMessage("<proposed_plan>\n# Pending\n</proposed_plan>")]);
		await harness.session.prompt("plan it");
		const state = harness.session.planState;
		if (state.status !== "awaitingApproval") throw new Error("Expected a pending plan");

		let releaseResponse: (() => void) | undefined;
		const responseGate = new Promise<void>((resolve) => {
			releaseResponse = resolve;
		});
		harness.setResponses([
			async () => {
				await responseGate;
				return fauxAssistantMessage("Still waiting for approval.");
			},
		]);
		const promptPromise = harness.session.prompt("Do not approve this through chat.");
		await Promise.resolve();
		await Promise.resolve();

		expect(() => harness.session.exitPlanMode()).toThrow("while the agent is running");
		await expect(harness.session.approvePlan(state.proposal.id)).rejects.toThrow("while the agent is running");
		expect(() => harness.session.cancelPlan(state.proposal.id)).toThrow("while the agent is running");

		releaseResponse?.();
		await promptPromise;
	});
});
