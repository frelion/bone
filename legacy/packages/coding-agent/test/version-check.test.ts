import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
	checkForNewBoneVersion,
	comparePackageVersions,
	getLatestBoneRelease,
	getLatestBoneVersion,
	isNewerPackageVersion,
} from "../src/utils/version-check.ts";

const originalUpdateUrl = process.env.BONE_UPDATE_URL;
const originalSkipVersionCheck = process.env.BONE_SKIP_VERSION_CHECK;
const originalOffline = process.env.BONE_OFFLINE;

afterEach(() => {
	vi.unstubAllGlobals();
	if (originalUpdateUrl === undefined) {
		delete process.env.BONE_UPDATE_URL;
	} else {
		process.env.BONE_UPDATE_URL = originalUpdateUrl;
	}
	if (originalSkipVersionCheck === undefined) {
		delete process.env.BONE_SKIP_VERSION_CHECK;
	} else {
		process.env.BONE_SKIP_VERSION_CHECK = originalSkipVersionCheck;
	}
	if (originalOffline === undefined) {
		delete process.env.BONE_OFFLINE;
	} else {
		process.env.BONE_OFFLINE = originalOffline;
	}
});

describe("version checks", () => {
	beforeEach(() => {
		process.env.BONE_UPDATE_URL = "https://updates.bone.test/latest-version";
	});

	it("does not make a version request unless a Bone endpoint is configured", async () => {
		delete process.env.BONE_UPDATE_URL;
		const fetchMock = vi.fn();
		vi.stubGlobal("fetch", fetchMock);

		await expect(getLatestBoneVersion("1.2.3")).resolves.toBeUndefined();
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it("compares package versions", () => {
		expect(comparePackageVersions("0.70.6", "0.70.5")).toBeGreaterThan(0);
		expect(comparePackageVersions("0.70.5", "0.70.5")).toBe(0);
		expect(comparePackageVersions("0.70.4", "0.70.5")).toBeLessThan(0);
		expect(comparePackageVersions("5.0.0-beta.20", "5.0.0-beta.9")).toBeGreaterThan(0);
		expect(isNewerPackageVersion("0.70.5", "0.70.5")).toBe(false);
		expect(isNewerPackageVersion("0.70.6", "0.70.5")).toBe(true);
	});

	it("returns only newer versions", async () => {
		const fetchMock = vi.fn(async () => Response.json({ version: "1.2.3" }));
		vi.stubGlobal("fetch", fetchMock);

		await expect(checkForNewBoneVersion("1.2.3")).resolves.toBeUndefined();
		await expect(checkForNewBoneVersion("1.2.2")).resolves.toEqual({ version: "1.2.3" });
	});

	it("uses the configured Bone version check endpoint with a Bone user agent", async () => {
		const fetchMock = vi.fn(async () => Response.json({ version: "1.2.4" }));
		vi.stubGlobal("fetch", fetchMock);

		await expect(getLatestBoneVersion("1.2.3")).resolves.toBe("1.2.4");
		expect(fetchMock).toHaveBeenCalledWith(
			"https://updates.bone.test/latest-version",
			expect.objectContaining({
				headers: expect.objectContaining({
					"User-Agent": expect.stringMatching(/^bone\/1\.2\.3 /),
					accept: "application/json",
				}),
			}),
		);
	});

	it("returns the active package metadata from the version check api", async () => {
		const fetchMock = vi.fn(async () =>
			Response.json({
				packageName: "@new-scope/pi",
				version: "1.2.4",
			}),
		);
		vi.stubGlobal("fetch", fetchMock);

		await expect(getLatestBoneRelease("1.2.3")).resolves.toEqual({
			packageName: "@new-scope/pi",
			version: "1.2.4",
		});
	});

	it("returns update notes from the version check api", async () => {
		const fetchMock = vi.fn(async () => Response.json({ note: " **Read this** ", version: "1.2.4" }));
		vi.stubGlobal("fetch", fetchMock);

		await expect(getLatestBoneRelease("1.2.3")).resolves.toEqual({ note: "**Read this**", version: "1.2.4" });
	});

	it("skips api calls when version checks are disabled", async () => {
		process.env.BONE_SKIP_VERSION_CHECK = "1";
		const fetchMock = vi.fn();
		vi.stubGlobal("fetch", fetchMock);

		await expect(getLatestBoneVersion("1.2.3")).resolves.toBeUndefined();
		expect(fetchMock).not.toHaveBeenCalled();
	});
});
