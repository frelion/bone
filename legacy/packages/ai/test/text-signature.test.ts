import { describe, expect, it } from "vitest";
import {
	encodeTextSignature,
	getTextContentPhase,
	isTextPhase,
	parseTextSignature,
} from "../src/utils/text-signature.ts";

describe("text signatures", () => {
	it.each(["commentary", "final_answer"] as const)("round-trips the %s phase", (phase) => {
		const signature = encodeTextSignature("message-1", phase);

		expect(parseTextSignature(signature)).toEqual({ id: "message-1", phase });
		expect(getTextContentPhase({ textSignature: signature })).toBe(phase);
	});

	it("preserves legacy plain signatures as provider IDs", () => {
		expect(parseTextSignature("legacy-message-id")).toEqual({ id: "legacy-message-id" });
		expect(getTextContentPhase({ textSignature: "legacy-message-id" })).toBeUndefined();
	});

	it.each([
		undefined,
		JSON.stringify({ v: 1, id: "message-1", phase: "unknown" }),
		JSON.stringify({ v: 1, phase: "commentary" }),
		JSON.stringify({ v: 2, id: "message-1", phase: "commentary" }),
		"{malformed",
	])("does not expose an invalid phase from %s", (textSignature) => {
		expect(getTextContentPhase({ textSignature })).toBeUndefined();
	});

	it("recognizes only supported phases", () => {
		expect(isTextPhase("commentary")).toBe(true);
		expect(isTextPhase("final_answer")).toBe(true);
		expect(isTextPhase("analysis")).toBe(false);
	});
});
