import { KeyEvent } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, describe, expect, test, vi } from "vitest";
import type { PlanProposal } from "../src/core/plan-mode.ts";
import {
	OpenTUIPlanReview,
	type OpenTUIPlanReviewResult,
} from "../src/modes/interactive/components/opentui-plan-review.ts";

const renderers = new Set<TestRendererSetup>();

async function createPlanReviewRenderer() {
	const setup = await createTestRenderer({ width: 84, height: 20, autoFocus: false, kittyKeyboard: true });
	renderers.add(setup);
	return setup;
}

function proposal(): PlanProposal {
	return {
		id: "plan-1",
		version: 2,
		content: "# Implement the migration",
		createdAt: new Date(0).toISOString(),
		sourceMessageId: "assistant-1",
	};
}

function key(name: string, modifiers: { ctrl?: boolean; shift?: boolean; meta?: boolean } = {}): KeyEvent {
	return new KeyEvent({
		name,
		ctrl: modifiers.ctrl ?? false,
		meta: modifiers.meta ?? false,
		shift: modifiers.shift ?? false,
		option: false,
		sequence: "",
		number: false,
		raw: "",
		eventType: "press",
		source: "raw",
	});
}

afterEach(() => {
	for (const setup of renderers) setup.renderer.destroy();
	renderers.clear();
});

describe("OpenTUIPlanReview", () => {
	test("approves or cancels from the action list", async () => {
		const setup = await createPlanReviewRenderer();
		const done = vi.fn<(result: OpenTUIPlanReviewResult) => void>();
		const review = new OpenTUIPlanReview(setup.renderer, proposal(), done);
		setup.renderer.root.add(review.root);
		review.focus();

		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Plan v2 is ready");
		await vi.waitFor(async () => {
			await setup.flush();
			expect(setup.captureCharFrame()).toContain("Implement the migration");
		});
		review.handleKey(key("enter"));
		expect(done).toHaveBeenCalledWith({ action: "approve" });

		const cancel = vi.fn<(result: OpenTUIPlanReviewResult) => void>();
		const cancelReview = new OpenTUIPlanReview(setup.renderer, proposal(), cancel);
		setup.renderer.root.add(cancelReview.root);
		cancelReview.handleKey(key("down"));
		cancelReview.handleKey(key("down"));
		cancelReview.handleKey(key("enter"));
		expect(cancel).toHaveBeenCalledWith({ action: "cancel" });
	});

	test("collects revision feedback and validates an empty submission inline", async () => {
		const setup = await createPlanReviewRenderer();
		const done = vi.fn<(result: OpenTUIPlanReviewResult) => void>();
		const review = new OpenTUIPlanReview(setup.renderer, proposal(), done);
		setup.renderer.root.add(review.root);
		review.focus();

		review.handleKey(key("down"));
		review.handleKey(key("enter"));
		expect(review.feedbackActive).toBe(true);
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Enter submit · Shift+Enter newline · Esc back");

		review.handleKey(key("s", { ctrl: true }));
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Revision feedback must not be empty.");
		expect(done).not.toHaveBeenCalled();

		await setup.mockInput.typeText("Keep the public API smaller");
		setup.mockInput.pressEnter();
		expect(done).toHaveBeenCalledWith({ action: "revise", feedback: "Keep the public API smaller" });
	});

	test("restores a feedback draft after the review is remounted", async () => {
		const setup = await createPlanReviewRenderer();
		const review = new OpenTUIPlanReview(setup.renderer, proposal(), vi.fn(), "Preserve the adapter boundary");
		setup.renderer.root.add(review.root);
		review.handleKey(key("down"));
		review.handleKey(key("enter"));

		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Preserve the adapter boundary");
		review.handleKey(key("escape"));
		await setup.flush();
		expect(review.feedbackActive).toBe(false);
		expect(review.getDraftFeedback()).toBe("Preserve the adapter boundary");
	});

	test("clears empty-feedback validation when returning to the action list", async () => {
		const setup = await createPlanReviewRenderer();
		const review = new OpenTUIPlanReview(setup.renderer, proposal(), vi.fn());
		setup.renderer.root.add(review.root);
		review.handleKey(key("down"));
		review.handleKey(key("enter"));
		review.handleKey(key("s", { ctrl: true }));
		await setup.flush();
		expect(setup.captureCharFrame()).toContain("Revision feedback must not be empty.");

		review.handleKey(key("escape"));
		await setup.flush();
		expect(setup.captureCharFrame()).not.toContain("Revision feedback must not be empty.");
		expect(setup.captureCharFrame()).toContain("Execute plan");
	});
});
