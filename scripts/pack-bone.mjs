#!/usr/bin/env bun

import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(scriptDir, "..");
const codingAgentDir = join(repoRoot, "packages", "coding-agent");
const packageDirectories = [
	{ name: "@frelion/bone-ai", directory: join(repoRoot, "packages", "ai") },
	{ name: "@frelion/bone-tui", directory: join(repoRoot, "packages", "tui") },
	{ name: "@frelion/bone-agent-core", directory: join(repoRoot, "packages", "agent") },
];

function run(command, args, cwd) {
	const result = spawnSync(command, args, { cwd, encoding: "utf8", stdio: ["inherit", "pipe", "inherit"] });
	if (result.status !== 0) {
		throw new Error(`Command failed: ${[command, ...args].join(" ")}`);
	}
	return result.stdout;
}

function parseOptions() {
	const args = process.argv.slice(2);
	let outputDir = resolve(repoRoot, "artifacts");
	let skipBuild = false;

	for (let index = 0; index < args.length; index++) {
		const arg = args[index];
		if (arg === "--skip-build") {
			skipBuild = true;
			continue;
		}
		if (arg === "--out" && args[index + 1]) {
			outputDir = resolve(process.cwd(), args[++index]);
			continue;
		}
		throw new Error("Usage: bun run pack:bone -- [--out <directory>] [--skip-build]");
	}

	return { outputDir, skipBuild };
}

function pack(directory, outputDir) {
	const packageJson = JSON.parse(readFileSync(join(directory, "package.json"), "utf8"));
	// The default artifacts directory is intentionally reused by local development,
	// so remove only this package's exact previous output.
	const filename = `${packageJson.name.replace("@", "").replace("/", "-")}-${packageJson.version}.tgz`;
	const destination = join(outputDir, filename);
	if (existsSync(destination)) rmSync(destination);
	const packed = spawnSync("bun", ["pm", "pack", "--ignore-scripts", "--quiet", "--filename", destination], {
		cwd: directory,
		encoding: "utf8",
		maxBuffer: 32 * 1024 * 1024,
		stdio: ["inherit", "pipe", "inherit"],
	});
	if (packed.status !== 0 || !existsSync(destination)) throw new Error(`bun pm pack failed for ${directory}`);
	return destination;
}

const { outputDir, skipBuild } = parseOptions();
mkdirSync(outputDir, { recursive: true });

if (!skipBuild) {
	run("bun", ["run", "build"], repoRoot);
}

const nativeAssets = join(codingAgentDir, "dist", "native");
run("bun", ["scripts/verify-semantic-native.mjs", "--root", nativeAssets], repoRoot);
run("bun", ["scripts/verify-bone-package-metadata.mjs", "--root", codingAgentDir], repoRoot);

const stagingDir = mkdtempSync(join(tmpdir(), "bone-pack-"));
const stagingPackageDir = join(stagingDir, "package");
const tarballsDir = join(stagingDir, "tarballs");
mkdirSync(stagingPackageDir, { recursive: true });
mkdirSync(tarballsDir, { recursive: true });

const internalTarballs = new Map(packageDirectories.map((pkg) => [pkg.name, pack(pkg.directory, tarballsDir)]));

for (const entry of ["dist", "docs", "examples"]) {
	const source = join(codingAgentDir, entry);
	if (existsSync(source)) {
		cpSync(source, join(stagingPackageDir, entry), { recursive: true });
	}
}
for (const entry of ["README.md", "CHANGELOG.md"]) {
	cpSync(join(codingAgentDir, entry), join(stagingPackageDir, entry));
}

run("bun", ["scripts/verify-semantic-native.mjs", "--root", join(stagingPackageDir, "dist", "native")], repoRoot);

const packageJson = JSON.parse(readFileSync(join(codingAgentDir, "package.json"), "utf8"));
const publishedDependencies = { ...packageJson.dependencies };
const bundledDependencies = packageDirectories.map((pkg) => pkg.name);
packageJson.bundleDependencies = bundledDependencies;
packageJson.dependencies = {
	...publishedDependencies,
	...Object.fromEntries(
		bundledDependencies.map((name) => {
			const tarball = internalTarballs.get(name);
			if (!tarball) throw new Error(`Missing tarball for ${name}`);
			return [name, `file:${tarball}`];
		}),
	),
};
writeFileSync(join(stagingPackageDir, "package.json"), `${JSON.stringify(packageJson, null, "\t")}\n`);
run("bun", ["scripts/verify-bone-package-metadata.mjs", "--root", stagingPackageDir], repoRoot);

run("bun", ["install", "--production", "--ignore-scripts", "--no-save"], stagingPackageDir);

packageJson.dependencies = publishedDependencies;
writeFileSync(join(stagingPackageDir, "package.json"), `${JSON.stringify(packageJson, null, "\t")}\n`);

const boneTarball = pack(stagingPackageDir, outputDir);
console.log(`Created self-contained Bone package: ${boneTarball}`);
