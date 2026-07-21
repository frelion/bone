import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { LOCKSTEP_PACKAGE_DIRS, syncWorkspaceVersions } from "./sync-versions.js";

function createRepo() {
	const root = mkdtempSync(join(tmpdir(), "bone-version-sync-"));
	const names = {
		agent: "@frelion/bone-agent-core",
		ai: "@frelion/bone-ai",
		"coding-agent": "@frelion/bone-coding-agent",
		orchestrator: "@frelion/bone-orchestrator",
		tui: "@frelion/bone-tui",
	};
	for (const directory of LOCKSTEP_PACKAGE_DIRS) {
		const packageDir = join(root, "packages", directory);
		mkdirSync(packageDir, { recursive: true });
		const dependencies = directory === "coding-agent" ? { "@frelion/bone-ai": "^0.0.8" } : undefined;
		writeFileSync(
			join(packageDir, "package.json"),
			`${JSON.stringify({ name: names[directory], version: "0.0.9", dependencies }, null, "\t")}\n`,
		);
	}
	const exampleDir = join(root, "packages", "coding-agent", "examples", "extensions", "example");
	mkdirSync(exampleDir, { recursive: true });
	writeFileSync(join(exampleDir, "package.json"), '{"name":"private-example","version":"7.8.9","private":true}\n');
	return root;
}

test("updates only lockstep package versions and internal dependency ranges", () => {
	const root = createRepo();
	try {
		const result = syncWorkspaceVersions({ root, target: "0.0.10" });
		assert.equal(result.targetVersion, "0.0.10");
		for (const directory of LOCKSTEP_PACKAGE_DIRS) {
			const pkg = JSON.parse(readFileSync(join(root, "packages", directory, "package.json"), "utf8"));
			assert.equal(pkg.version, "0.0.10");
		}
		const codingAgent = JSON.parse(
			readFileSync(join(root, "packages", "coding-agent", "package.json"), "utf8"),
		);
		assert.equal(codingAgent.dependencies["@frelion/bone-ai"], "^0.0.10");
		const example = JSON.parse(
			readFileSync(
				join(root, "packages", "coding-agent", "examples", "extensions", "example", "package.json"),
				"utf8",
			),
		);
		assert.equal(example.version, "7.8.9");
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("supports a no-write dry run", () => {
	const root = createRepo();
	try {
		const path = join(root, "packages", "ai", "package.json");
		const before = readFileSync(path, "utf8");
		const result = syncWorkspaceVersions({ root, target: "0.0.10", write: false });
		assert.ok(result.changes.length > 0);
		assert.equal(readFileSync(path, "utf8"), before);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});

test("rejects version downgrades", () => {
	const root = createRepo();
	try {
		assert.throws(() => syncWorkspaceVersions({ root, target: "0.0.8" }), /lower than current version/);
	} finally {
		rmSync(root, { recursive: true, force: true });
	}
});
