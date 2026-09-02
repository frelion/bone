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
	{ name: "@frelion/bone-session", directory: join(repoRoot, "packages", "session") },
	{ name: "@frelion/bone-protocol", directory: join(repoRoot, "packages", "protocol") },
	{ name: "@frelion/bone-images", directory: join(repoRoot, "packages", "images") },
	{ name: "@frelion/bone-forge", directory: join(repoRoot, "packages", "forge") },
	{ name: "@frelion/bone-memory", directory: join(repoRoot, "packages", "memory") },
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

function resolveInstalledPackage(installDirectory, codingAgentDirectory, packageDirectory) {
	const candidates = [
		join(installDirectory, "node_modules", "@frelion", packageDirectory),
		join(codingAgentDirectory, "node_modules", "@frelion", packageDirectory),
	];
	const resolved = candidates.find((candidate) => existsSync(candidate));
	if (!resolved) throw new Error(`Packed Bone is missing required package @frelion/${packageDirectory}`);
	return resolved;
}

function verifyPackedBone(tarball) {
	const extractionDirectory = mkdtempSync(join(tmpdir(), "bone-packed-extract-"));
	run("tar", ["-xzf", tarball, "-C", extractionDirectory], repoRoot);
	const installedCodingAgent = join(extractionDirectory, "package");
	if (!existsSync(join(installedCodingAgent, "package.json"))) {
		throw new Error("Packed Bone archive is missing package metadata");
	}
	for (const pkg of packageDirectories) {
		const packageDirectory = pkg.name.slice("@frelion/".length);
		const installedPackage = resolveInstalledPackage(extractionDirectory, installedCodingAgent, packageDirectory);
		const metadata = JSON.parse(readFileSync(join(installedPackage, "package.json"), "utf8"));
		if (metadata.name !== pkg.name) throw new Error(`Packed Bone contains invalid metadata for ${pkg.name}`);
		if (!metadata.main || !existsSync(join(installedPackage, metadata.main))) {
			throw new Error(`Packed Bone is missing the runtime entrypoint for ${pkg.name}`);
		}
	}
	const installedMemory = resolveInstalledPackage(extractionDirectory, installedCodingAgent, "bone-memory");
	run("bun", ["scripts/verify-semantic-native.mjs", "--root", join(installedCodingAgent, "dist", "native")], repoRoot);
	run("bun", ["scripts/verify-semantic-native.mjs", "--root", join(installedMemory, "native")], repoRoot);
	run(
		"bun",
		[
			"-e",
			"import { getLocalEmbeddingNativeLibraryPath } from '@frelion/bone-memory'; console.log(getLocalEmbeddingNativeLibraryPath());",
		],
		installedCodingAgent,
	);
	run("bun", ["-e", "await import('@frelion/bone-ai');"], installedCodingAgent);
	run(
		"bun",
		[
			"-e",
			"try { await import('@frelion/bone-ai/compat'); throw new Error('legacy compat entrypoint is still public'); } catch (error) { if (error instanceof Error && error.message === 'legacy compat entrypoint is still public') throw error; }",
		],
		installedCodingAgent,
	);
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
const publishedOverrides = { ...packageJson.overrides };
const bundledDependencies = packageDirectories.map((pkg) => pkg.name);
const bundledSpecifiers = Object.fromEntries(
	bundledDependencies.map((name) => {
		const tarball = internalTarballs.get(name);
		if (!tarball) throw new Error(`Missing tarball for ${name}`);
		return [name, `file:${tarball}`];
	}),
);
packageJson.bundledDependencies = bundledDependencies;
packageJson.dependencies = { ...publishedDependencies, ...bundledSpecifiers };
packageJson.overrides = { ...publishedOverrides, ...bundledSpecifiers };
writeFileSync(join(stagingPackageDir, "package.json"), `${JSON.stringify(packageJson, null, "\t")}\n`);
run("bun", ["scripts/verify-bone-package-metadata.mjs", "--root", stagingPackageDir], repoRoot);

run("bun", ["install", "--production", "--ignore-scripts", "--no-save"], stagingPackageDir);

packageJson.dependencies = publishedDependencies;
packageJson.overrides = publishedOverrides;
writeFileSync(join(stagingPackageDir, "package.json"), `${JSON.stringify(packageJson, null, "\t")}\n`);

const boneTarball = pack(stagingPackageDir, outputDir);
verifyPackedBone(boneTarball);
console.log(`Created and verified self-contained Bone package: ${boneTarball}`);
