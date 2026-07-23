#!/usr/bin/env bun

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export const LOCKSTEP_PACKAGE_DIRS = ["ai", "agent", "tui", "coding-agent", "orchestrator"];
const SEMVER_RE = /^\d+\.\d+\.\d+$/;

function compareVersions(left, right) {
	const leftParts = left.split(".").map(Number);
	const rightParts = right.split(".").map(Number);
	for (let index = 0; index < 3; index++) {
		const difference = leftParts[index] - rightParts[index];
		if (difference !== 0) return difference;
	}
	return 0;
}

function readPackages(root) {
	return LOCKSTEP_PACKAGE_DIRS.map((directory) => {
		const path = join(root, "packages", directory, "package.json");
		return { directory, path, data: JSON.parse(readFileSync(path, "utf8")) };
	});
}

function updateDependencyGroup(group, versions, targetVersion, changes, packageName, field) {
	if (!group) return;
	for (const [dependencyName, currentSpec] of Object.entries(group)) {
		if (!versions.has(dependencyName)) continue;
		const nextSpec = `^${targetVersion}`;
		if (currentSpec === nextSpec) continue;
		group[dependencyName] = nextSpec;
		changes.push(`${packageName} ${field}.${dependencyName}: ${currentSpec} -> ${nextSpec}`);
	}
}

export function syncWorkspaceVersions({ root, target, write = true }) {
	const packages = readPackages(root);
	const currentVersions = new Set(packages.map((pkg) => pkg.data.version));
	if (currentVersions.size !== 1) {
		throw new Error(`Lockstep package versions differ: ${[...currentVersions].join(", ")}`);
	}

	const currentVersion = packages[0].data.version;
	const targetVersion = target ?? currentVersion;
	if (!SEMVER_RE.test(targetVersion)) throw new Error(`Invalid target version: ${targetVersion}`);
	if (compareVersions(targetVersion, currentVersion) < 0) {
		throw new Error(`Target version ${targetVersion} is lower than current version ${currentVersion}`);
	}

	const versions = new Set(packages.map((pkg) => pkg.data.name));
	const changes = [];
	for (const pkg of packages) {
		if (pkg.data.version !== targetVersion) {
			changes.push(`${pkg.data.name} version: ${pkg.data.version} -> ${targetVersion}`);
			pkg.data.version = targetVersion;
		}
		updateDependencyGroup(pkg.data.dependencies, versions, targetVersion, changes, pkg.data.name, "dependencies");
		updateDependencyGroup(pkg.data.devDependencies, versions, targetVersion, changes, pkg.data.name, "devDependencies");
	}

	if (write) {
		for (const pkg of packages) writeFileSync(pkg.path, `${JSON.stringify(pkg.data, null, "\t")}\n`);
	}

	return { changes, currentVersion, targetVersion };
}

function parseArgs(args) {
	const options = { check: false, dryRun: false, root: resolve(dirname(fileURLToPath(import.meta.url)), ".."), target: undefined };
	for (let index = 0; index < args.length; index++) {
		const arg = args[index];
		if (arg === "--check") options.check = true;
		else if (arg === "--dry-run") options.dryRun = true;
		else if (arg === "--target") options.target = args[++index];
		else if (arg === "--root") options.root = resolve(args[++index]);
		else throw new Error(`Unknown argument: ${arg}`);
	}
	if (options.check && options.target) throw new Error("--check cannot be combined with --target");
	return options;
}

function main() {
	const options = parseArgs(process.argv.slice(2));
	const result = syncWorkspaceVersions({
		root: options.root,
		target: options.target,
		write: !options.check && !options.dryRun,
	});
	if (result.changes.length === 0) {
		console.log(`Lockstep workspaces are synchronized at ${result.targetVersion}.`);
		return;
	}
	for (const change of result.changes) console.log(change);
	if (options.check) {
		console.error("Lockstep workspace versions or dependency ranges are out of sync.");
		process.exitCode = 1;
	} else if (options.dryRun) {
		console.log("Dry run only; no files changed.");
	} else {
		console.log(`Synchronized lockstep workspaces at ${result.targetVersion}.`);
	}
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
	try {
		main();
	} catch (error) {
		console.error(error instanceof Error ? error.message : String(error));
		process.exitCode = 1;
	}
}
