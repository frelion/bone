#!/usr/bin/env node

import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";

function readJson(path) {
	try {
		return JSON.parse(readFileSync(path, "utf8"));
	} catch (error) {
		throw new Error(`Could not read ${path}: ${error instanceof Error ? error.message : String(error)}`);
	}
}

const args = process.argv.slice(2);
if (args.length !== 2 || args[0] !== "--root") {
	throw new Error("Usage: node scripts/verify-bone-package-metadata.mjs --root <coding-agent-package-directory>");
}

const root = resolve(args[1]);
const packagePath = join(root, "package.json");
const distPackagePath = join(root, "dist", "package.json");
const cliPath = join(root, "dist", "bun", "cli.js");
if (!existsSync(cliPath)) throw new Error(`Bone package is missing its CLI entrypoint: ${cliPath}`);
if (existsSync(distPackagePath)) {
	throw new Error(
		`Bun-distributed Bone package must not contain ${distPackagePath}; it makes runtime asset resolution point at dist/dist`,
	);
}

const packageJson = readJson(packagePath);
if (packageJson.boneConfig?.name !== "bone" || packageJson.boneConfig?.configDir !== ".bone") {
	throw new Error("Bone package is missing its expected Bone boneConfig metadata");
}

if (packageJson.bin?.bone !== "dist/bun/cli.js") {
	throw new Error("Bone package must expose dist/bun/cli.js as its CLI entrypoint");
}
if (!packageJson.engines?.bun || packageJson.engines.node) {
	throw new Error("Bone package must declare Bun, and must not declare Node.js, as its runtime");
}
console.log(`Verified Bun Bone package metadata: ${packageJson.name}@${packageJson.version}`);
