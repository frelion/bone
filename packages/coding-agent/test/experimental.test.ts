import { afterEach, describe, expect, it } from "vitest";
import { areExperimentalFeaturesEnabled } from "../src/core/experimental.ts";

describe("areExperimentalFeaturesEnabled", () => {
	const originalBoneExperimental = process.env.BONE_EXPERIMENTAL;

	afterEach(() => {
		if (originalBoneExperimental === undefined) {
			delete process.env.BONE_EXPERIMENTAL;
		} else {
			process.env.BONE_EXPERIMENTAL = originalBoneExperimental;
		}
	});

	it("returns false when BONE_EXPERIMENTAL is unset", () => {
		delete process.env.BONE_EXPERIMENTAL;

		expect(areExperimentalFeaturesEnabled()).toBe(false);
	});

	it("returns false when BONE_EXPERIMENTAL is empty", () => {
		process.env.BONE_EXPERIMENTAL = "";

		expect(areExperimentalFeaturesEnabled()).toBe(false);
	});

	it("returns true when BONE_EXPERIMENTAL is set to 1", () => {
		process.env.BONE_EXPERIMENTAL = "1";

		expect(areExperimentalFeaturesEnabled()).toBe(true);
	});

	it("returns false when BONE_EXPERIMENTAL is set to 0", () => {
		process.env.BONE_EXPERIMENTAL = "0";

		expect(areExperimentalFeaturesEnabled()).toBe(false);
	});

	it("returns false when BONE_EXPERIMENTAL is set to a non-1 value", () => {
		process.env.BONE_EXPERIMENTAL = "true";

		expect(areExperimentalFeaturesEnabled()).toBe(false);
	});
});
