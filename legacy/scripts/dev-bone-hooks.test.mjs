import assert from "node:assert/strict";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { isBoneManagedHooksPath } from "./dev-bone-hooks.mjs";

const testRoot = join(tmpdir(), `bone-dev-hooks-${process.pid}`);
const repoRoot = join(testRoot, "repo");
const devRoot = join(testRoot, "dev");
const managedHooksPath = join(devRoot, "hooks", "repository");
mkdirSync(managedHooksPath, { recursive: true });
writeFileSync(join(managedHooksPath, "config.json"), "{}\n");

after(() => rmSync(testRoot, { recursive: true, force: true }));

test("recognizes an installed Bone hook from any worktree", () => {
	assert.equal(isBoneManagedHooksPath(managedHooksPath, { repoRoot, devRoot }), true);
});

test("does not treat Husky or incomplete Bone hook paths as managed", () => {
	assert.equal(isBoneManagedHooksPath(".husky/_", { repoRoot, devRoot }), false);
	assert.equal(isBoneManagedHooksPath(join(devRoot, "hooks", "missing"), { repoRoot, devRoot }), false);
	assert.equal(isBoneManagedHooksPath(join(testRoot, "dev-other", "hooks", "repository"), { repoRoot, devRoot }), false);
});
