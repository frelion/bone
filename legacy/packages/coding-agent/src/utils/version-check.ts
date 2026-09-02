import { compare, valid } from "semver";
import { getUpdateCheckUrl } from "../config.ts";
import { getPiUserAgent } from "./pi-user-agent.ts";

const DEFAULT_VERSION_CHECK_TIMEOUT_MS = 10000;

export interface LatestBoneRelease {
	version: string;
	packageName?: string;
	note?: string;
}

export function comparePackageVersions(leftVersion: string, rightVersion: string): number | undefined {
	const left = valid(leftVersion.trim());
	const right = valid(rightVersion.trim());
	if (!left || !right) {
		return undefined;
	}
	return compare(left, right);
}

export function isNewerPackageVersion(candidateVersion: string, currentVersion: string): boolean {
	const comparison = comparePackageVersions(candidateVersion, currentVersion);
	if (comparison !== undefined) {
		return comparison > 0;
	}
	return candidateVersion.trim() !== currentVersion.trim();
}

export async function getLatestBoneRelease(
	currentVersion: string,
	options: { timeoutMs?: number } = {},
): Promise<LatestBoneRelease | undefined> {
	if (process.env.BONE_SKIP_VERSION_CHECK || process.env.BONE_OFFLINE) {
		return undefined;
	}
	const latestVersionUrl = getUpdateCheckUrl();
	if (!latestVersionUrl) return undefined;

	const response = await fetch(latestVersionUrl, {
		headers: {
			"User-Agent": getPiUserAgent(currentVersion),
			accept: "application/json",
		},
		signal: AbortSignal.timeout(options.timeoutMs ?? DEFAULT_VERSION_CHECK_TIMEOUT_MS),
	});
	if (!response.ok) return undefined;

	const data = (await response.json()) as {
		packageName?: unknown;
		version?: unknown;
		note?: unknown;
	};
	if (typeof data.version !== "string" || !data.version.trim()) {
		return undefined;
	}
	const packageName =
		typeof data.packageName === "string" && data.packageName.trim() ? data.packageName.trim() : undefined;
	const note = typeof data.note === "string" && data.note.trim() ? data.note.trim() : undefined;
	return {
		version: data.version.trim(),
		packageName,
		...(note ? { note } : {}),
	};
}

export async function getLatestBoneVersion(
	currentVersion: string,
	options: { timeoutMs?: number } = {},
): Promise<string | undefined> {
	return (await getLatestBoneRelease(currentVersion, options))?.version;
}

export async function checkForNewBoneVersion(currentVersion: string): Promise<LatestBoneRelease | undefined> {
	try {
		const latestRelease = await getLatestBoneRelease(currentVersion);
		if (latestRelease && isNewerPackageVersion(latestRelease.version, currentVersion)) {
			return latestRelease;
		}
		return undefined;
	} catch {
		return undefined;
	}
}
