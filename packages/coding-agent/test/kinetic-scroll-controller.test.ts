import { describe, expect, it } from "vitest";
import { KineticScrollController } from "../src/modes/interactive/components/kinetic-scroll-controller.ts";

function collectSteps(controller: KineticScrollController, from: number, until: number): number {
	let lines = 0;
	for (let now = from; now <= until; now += 16) {
		lines += controller.advance(now)?.lineCount ?? 0;
	}
	return lines;
}

describe("KineticScrollController", () => {
	it("moves immediately, then eases a single wheel event over a short tail", () => {
		const controller = new KineticScrollController();
		expect(controller.receive("up", 0)).toEqual({ direction: "up", lineCount: 1 });

		const tailLines = collectSteps(controller, 16, 400);
		expect(tailLines).toBeGreaterThanOrEqual(1);
		expect(tailLines).toBeLessThanOrEqual(4);
		expect(controller.active).toBe(false);
	});

	it("builds substantially more speed for dense trackpad input without a FIFO backlog", () => {
		const slow = new KineticScrollController();
		slow.receive("up", 0);
		const slowTail = collectSteps(slow, 16, 400);

		const fast = new KineticScrollController();
		fast.receive("up", 0);
		fast.receive("up", 10);
		fast.receive("up", 20);
		fast.receive("up", 30);
		const fastTail = collectSteps(fast, 46, 400);

		expect(fastTail).toBeGreaterThan(slowTail * 2);
	});

	it("reverses immediately instead of finishing stale momentum in the old direction", () => {
		const controller = new KineticScrollController();
		controller.receive("up", 0);
		controller.receive("up", 10);
		expect(controller.receive("down", 20)).toEqual({ direction: "down", lineCount: 1 });

		const nextStep = controller.advance(52);
		expect(nextStep?.direction).toBe("down");
	});

	it("cancels all residual movement", () => {
		const controller = new KineticScrollController();
		controller.receive("up", 0);
		controller.receive("up", 10);
		controller.cancel();

		expect(controller.active).toBe(false);
		expect(controller.advance(100)).toBeUndefined();
	});
});
