import { describe, expect, it } from "vitest";
import { appendPlanModeInstructions, parseProposedPlan, removeProposedPlanBlock } from "../src/core/plan-mode.ts";

describe("Plan mode proposal parsing", () => {
	it("accepts one complete non-empty proposal", () => {
		expect(parseProposedPlan("before\n<proposed_plan>\n# Plan\n\nDo the work.\n</proposed_plan>\nafter")).toEqual({
			status: "valid",
			content: "# Plan\n\nDo the work.",
		});
	});

	it("does not treat ordinary responses as proposals", () => {
		expect(parseProposedPlan("Which behavior should be preserved?")).toEqual({ status: "none" });
	});

	it.each([
		["empty", "<proposed_plan>\n\n</proposed_plan>"],
		["missing close", "<proposed_plan>\n# Plan"],
		["inline tags", "<proposed_plan># Plan</proposed_plan>"],
		["multiple blocks", "<proposed_plan>\n# One\n</proposed_plan>\n<proposed_plan>\n# Two\n</proposed_plan>"],
	])("rejects %s", (_name, text) => {
		expect(parseProposedPlan(text).status).toBe("invalid");
	});

	it("removes a completed proposal from normal assistant rendering", () => {
		expect(removeProposedPlanBlock("Intro\n<proposed_plan>\n# Plan\n</proposed_plan>\nOutro")).toBe("Intro\n\nOutro");
	});

	it("appends built-in instructions after the existing prompt", () => {
		const prompt = appendPlanModeInstructions("base prompt");
		expect(prompt).toMatch(/^base prompt/);
		expect(prompt).toContain("You are in Plan Mode");
		expect(prompt).toContain("<proposed_plan>");
	});
});
