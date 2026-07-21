import { describe, expect, it, vi } from "vitest";
import { InteractiveMode } from "../src/modes/interactive/interactive-mode.ts";
import { initTheme, theme } from "../src/modes/interactive/theme/theme.ts";

type InteractiveModeInternals = {
	session: FakeSession;
	sessionHost: { current: { session: FakeSession } };

	pendingPlanApprovalId?: string;
	reviewingPlanProposalId?: string;
	showExtensionSelector: ReturnType<typeof vi.fn>;
	showExtensionEditor: ReturnType<typeof vi.fn>;
	showStatus: ReturnType<typeof vi.fn>;
	showError: ReturnType<typeof vi.fn>;
	updatePlanModeStatus: ReturnType<typeof vi.fn>;
	ui: { requestRender: ReturnType<typeof vi.fn> };
	editor: { borderColor?: (text: string) => string };
	isBashMode: boolean;
	reviewPendingPlan(): Promise<void>;
	updateEditorBorderColor(): void;
};

type FakeSession = {
	collaborationMode: "default" | "plan";
	planState: { status: "planning" } | { status: "awaitingApproval"; proposal: { id: string; version: number } };
	approvePlan: ReturnType<typeof vi.fn>;
	revisePlan: ReturnType<typeof vi.fn>;
	cancelPlan: ReturnType<typeof vi.fn>;
	thinkingLevel: "off";
};

function createMode(): InteractiveModeInternals {
	const mode = Object.create(InteractiveMode.prototype) as InteractiveModeInternals;
	const session: FakeSession = {
		collaborationMode: "plan",
		planState: { status: "awaitingApproval", proposal: { id: "plan-1", version: 1 } },
		approvePlan: vi.fn(),
		revisePlan: vi.fn(),
		cancelPlan: vi.fn(() => {
			session.planState = { status: "planning" };
		}),
		thinkingLevel: "off",
	};
	mode.sessionHost = { current: { session } };
	mode.showExtensionSelector = vi.fn();
	mode.showExtensionEditor = vi.fn();
	mode.showStatus = vi.fn();
	mode.showError = vi.fn();
	mode.updatePlanModeStatus = vi.fn();
	mode.ui = { requestRender: vi.fn() };
	mode.editor = {};
	mode.isBashMode = false;
	return mode;
}

describe("Interactive Plan mode", () => {
	it("requires an explicit approval action after the selector is dismissed", async () => {
		const mode = createMode();
		mode.showExtensionSelector.mockResolvedValueOnce(undefined).mockResolvedValueOnce("Cancel plan");

		await mode.reviewPendingPlan();

		expect(mode.showExtensionSelector).toHaveBeenCalledTimes(2);
		expect(mode.session.cancelPlan).toHaveBeenCalledTimes(1);
	});

	it("returns to approval when revision feedback is dismissed", async () => {
		const mode = createMode();
		mode.showExtensionSelector.mockResolvedValueOnce("Revise plan").mockResolvedValueOnce("Cancel plan");
		mode.showExtensionEditor.mockResolvedValueOnce(undefined);

		await mode.reviewPendingPlan();

		expect(mode.showExtensionSelector).toHaveBeenCalledTimes(2);
		expect(mode.session.revisePlan).not.toHaveBeenCalled();
		expect(mode.session.cancelPlan).toHaveBeenCalledTimes(1);
	});

	it("keeps one approval owner and executes the replacement proposal after revision", async () => {
		const mode = createMode();
		mode.showExtensionSelector.mockResolvedValueOnce("Revise plan").mockResolvedValueOnce("Execute plan");
		mode.showExtensionEditor.mockResolvedValueOnce("Use a different implementation.");
		mode.session.revisePlan = vi.fn(async () => {
			const concurrentReview = mode.reviewPendingPlan();
			mode.session.planState = { status: "awaitingApproval", proposal: { id: "plan-2", version: 2 } };
			await concurrentReview;
		});
		mode.session.approvePlan = vi.fn(async (proposalId: string) => {
			expect(proposalId).toBe("plan-2");
			mode.session.planState = { status: "planning" };
		});

		await mode.reviewPendingPlan();

		expect(mode.showExtensionSelector).toHaveBeenCalledTimes(2);
		expect(mode.session.revisePlan).toHaveBeenCalledWith("plan-1", "Use a different implementation.");
		expect(mode.session.approvePlan).toHaveBeenCalledWith("plan-2");
	});

	it("uses distinct Plan and approval editor borders", () => {
		initTheme("dark");
		const mode = createMode();

		mode.session.planState = { status: "planning" };
		mode.updateEditorBorderColor();
		expect(mode.editor.borderColor?.("border")).toBe(theme.fg("borderAccent", "border"));

		mode.session.planState = { status: "awaitingApproval", proposal: { id: "plan-1", version: 1 } };
		mode.updateEditorBorderColor();
		expect(mode.editor.borderColor?.("border")).toBe(theme.fg("warning", "border"));
	});
});
