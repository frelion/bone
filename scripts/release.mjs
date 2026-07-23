#!/usr/bin/env bun

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { syncWorkspaceVersions } from "./sync-versions.js";

const root = process.cwd();
const statePath = join(root, ".git", "bone-release-state.json");
const changelogs = ["ai", "agent", "tui", "coding-agent", "orchestrator"].map(
	(packageName) => `packages/${packageName}/CHANGELOG.md`,
);
const SEMVER_RE = /^\d+\.\d+\.\d+$/;
const releasePathPatterns = [
	/^bun\.lock$/,
	/^packages\/(ai|agent|tui|coding-agent|orchestrator)\/package\.json$/,
	/^packages\/(ai|agent|tui|coding-agent|orchestrator)\/CHANGELOG\.md$/,
];

function run(command, args, options = {}) {
	console.log(`$ ${[command, ...args].join(" ")}`);
	return execFileSync(command, args, {
		cwd: root,
		encoding: "utf8",
		stdio: options.capture ? ["inherit", "pipe", "inherit"] : "inherit",
		env: { ...process.env, ...options.env },
	});
}

function capture(command, args) {
	return run(command, args, { capture: true }).trimEnd();
}

function getVersion() {
	return JSON.parse(readFileSync(join(root, "packages/ai/package.json"), "utf8")).version;
}

function requireVersion(version) {
	if (!SEMVER_RE.test(version ?? "")) throw new Error("A target version such as 0.0.10 is required.");
	return version;
}

function changedPaths() {
	const output = capture("git", ["status", "--porcelain"]);
	if (!output) return [];
	return output.split("\n").map((line) => line.slice(3).trim());
}

function assertClean() {
	const paths = changedPaths();
	if (paths.length > 0) throw new Error(`Working tree must be clean:\n${paths.map((path) => `  ${path}`).join("\n")}`);
}

function assertReleaseChanges(paths) {
	const unexpected = paths.filter((path) => !releasePathPatterns.some((pattern) => pattern.test(path)));
	if (unexpected.length > 0) {
		throw new Error(`Unexpected files in prepared release:\n${unexpected.map((path) => `  ${path}`).join("\n")}`);
	}
}

function updateChangelogs(version) {
	const date = new Date().toISOString().slice(0, 10);
	for (const path of changelogs) {
		const content = readFileSync(join(root, path), "utf8");
		if (content.includes(`## [${version}]`)) continue;
		if (!content.includes("## [Unreleased]")) throw new Error(`${path} has no [Unreleased] section`);
		writeFileSync(join(root, path), content.replace("## [Unreleased]", `## [${version}] - ${date}`));
	}
}

function addNextUnreleasedSections() {
	for (const path of changelogs) {
		const content = readFileSync(join(root, path), "utf8");
		if (content.includes("## [Unreleased]")) continue;
		writeFileSync(join(root, path), content.replace(/^# Changelog\n\n/, "# Changelog\n\n## [Unreleased]\n\n"));
	}
}

function writeState(state) {
	writeFileSync(statePath, `${JSON.stringify(state, null, 2)}\n`);
}

function readState(version) {
	if (!existsSync(statePath)) throw new Error(`No prepared release state for ${version}. Run release:prepare first.`);
	const state = JSON.parse(readFileSync(statePath, "utf8"));
	if (state.version !== version) throw new Error(`Prepared release is ${state.version}, not ${version}.`);
	return state;
}

function verifyWorkspaceAndLocks() {
	const syncResult = syncWorkspaceVersions({ root, write: false });
	if (syncResult.changes.length > 0) throw new Error(syncResult.changes.join("\n"));
	run("bun", ["install", "--frozen-lockfile", "--ignore-scripts", "--dry-run"]);
}

function doctor({ requireClean = true, requirePublishTools = false } = {}) {
	if (requireClean) assertClean();
	for (const command of ["git", "bun", ...(requirePublishTools ? ["gh"] : [])]) {
		run(command, ["--version"]);
	}
	verifyWorkspaceAndLocks();
	console.log(`Release prerequisites are valid at ${getVersion()}.`);
}

function prepare(version, { dryRun = false } = {}) {
	const paths = changedPaths();
	if (paths.length === 0) {
		doctor({ requireClean: false });
	} else {
		if (dryRun) throw new Error("A prepare dry run requires a clean working tree.");
		const state = readState(version);
		if (state.phase !== "preparing") throw new Error(`Release ${version} is already in phase ${state.phase}.`);
		assertReleaseChanges(paths);
		console.log(`Resuming release ${version} from the preparing phase.`);
	}
	if (!dryRun) writeState({ phase: "preparing", version });
	const result = syncWorkspaceVersions({ root, target: version, write: !dryRun });
	console.log(result.changes.join("\n"));
	if (dryRun) {
		console.log(`Dry run complete for ${version}; no files changed.`);
		return;
	}
	run("bun", ["install", "--lockfile-only", "--ignore-scripts"]);
	updateChangelogs(version);
	verifyWorkspaceAndLocks();
	run("bun", ["run", "check"]);
	run("./test.sh", []);
	const preparedPaths = changedPaths();
	assertReleaseChanges(preparedPaths);
	writeState({ phase: "prepared", version });
	console.log(`Release ${version} is prepared but not committed, tagged, or pushed.`);
}

function waitForRun(workflow, ref, { commit, retryFailed = false } = {}) {
	let runInfo;
	for (let attempt = 0; attempt < 120; attempt++) {
		const args = [
			"run",
			"list",
			"--workflow",
			workflow,
			"--limit",
			"1",
			"--json",
			"databaseId,status,conclusion,url",
		];
		if (commit) args.push("--commit", commit);
		else args.push("--branch", ref);
		const output = capture("gh", args);
		const runs = JSON.parse(output || "[]");
		if (runs[0]) {
			runInfo = runs[0];
			break;
		}
		Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 5000);
	}
	if (!runInfo) throw new Error(`No ${workflow} run found for ${ref}.`);
	if (runInfo.status === "completed" && runInfo.conclusion !== "success" && retryFailed) {
		run("gh", ["run", "rerun", String(runInfo.databaseId), "--failed"]);
	}
	run("gh", ["run", "watch", String(runInfo.databaseId), "--exit-status"]);
	return runInfo.url;
}

function publish(version) {
	let state = readState(version);
	if (state.phase === "prepared") {
		const paths = changedPaths();
		assertReleaseChanges(paths);
		if (getVersion() !== version) throw new Error(`Package version is ${getVersion()}, expected ${version}.`);
		verifyWorkspaceAndLocks();
		run("git", ["add", "--", ...paths]);
		run("git", ["commit", "-m", `Release v${version}`]);
		state = { phase: "release-committed", releaseCommit: capture("git", ["rev-parse", "HEAD"]), version };
		writeState(state);
	}
	if (state.phase === "release-committed") {
		assertClean();
		addNextUnreleasedSections();
		run("git", ["add", "--", ...changelogs]);
		run("git", ["commit", "-m", "Add [Unreleased] section for next cycle"]);
		state = {
			headCommit: capture("git", ["rev-parse", "HEAD"]),
			phase: "next-cycle-committed",
			releaseCommit: state.releaseCommit,
			version,
		};
		writeState(state);
	}
	assertClean();
	const headCommit = capture("git", ["rev-parse", "HEAD"]);
	const nativeInputsChanged = Boolean(
		capture("git", [
			"diff",
			"--name-only",
			"origin/main...HEAD",
			"--",
			".github/workflows/warm-native-cache.yml",
			"scripts/build-semantic-native.sh",
			"packages/coding-agent/native/CMakeLists.txt",
			"packages/coding-agent/native/bone-embed-addon.cpp",
			"patches/crispembed-bone-mmap.patch",
		]),
	);
	const tag = `v${version}`;
	if (state.phase === "next-cycle-committed") {
		run("git", ["push", "origin", "main"]);
		waitForRun("ci.yml", "main", { commit: headCommit, retryFailed: true });
		if (nativeInputsChanged) {
			waitForRun("warm-native-cache.yml", "main", { commit: headCommit, retryFailed: true });
		}
		const remoteTag = capture("git", ["ls-remote", "--tags", "origin", tag]);
		if (remoteTag) throw new Error(`Remote tag ${tag} already exists before this publish reached the tag phase.`);
		run("git", ["tag", tag, state.releaseCommit]);
		run("git", ["push", "origin", tag]);
		state = { ...state, phase: "tagged" };
		writeState(state);
	}
	const remoteTagCommit = capture("git", ["ls-remote", "--tags", "origin", tag]).split(/\s+/)[0];
	if (remoteTagCommit !== state.releaseCommit) {
		throw new Error(`Remote tag ${tag} does not point to prepared release commit ${state.releaseCommit}.`);
	}
	const releaseRun = waitForRun("build-binaries.yml", tag, { retryFailed: true });
	rmSync(statePath, { force: true });
	console.log(`Published ${tag}: ${releaseRun}`);
}

function usage() {
	console.log("Usage: release.mjs doctor | prepare <x.y.z> [--dry-run] | publish <x.y.z>");
}

try {
	const [command, versionArg, ...flags] = process.argv.slice(2);
	if (command === "doctor") doctor({ requirePublishTools: false });
	else if (command === "prepare") {
		const unknownFlags = flags.filter((flag) => flag !== "--dry-run");
		if (unknownFlags.length > 0) throw new Error(`Unknown arguments: ${unknownFlags.join(", ")}`);
		prepare(requireVersion(versionArg), { dryRun: flags.includes("--dry-run") });
	}
	else if (command === "publish") {
		doctor({ requireClean: false, requirePublishTools: true });
		publish(requireVersion(versionArg));
	} else {
		usage();
		process.exitCode = 1;
	}
} catch (error) {
	console.error(error instanceof Error ? error.message : String(error));
	process.exitCode = 1;
}
