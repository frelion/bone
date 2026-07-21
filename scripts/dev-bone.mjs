#!/usr/bin/env node

import {
	chmodSync,
	cpSync,
	existsSync,
	lstatSync,
	mkdirSync,
	readFileSync,
	readlinkSync,
	readdirSync,
	realpathSync,
	renameSync,
	rmSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, relative, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const repoRoot = resolve(dirname(scriptPath), "..");
const codingAgentDir = join(repoRoot, "packages", "coding-agent");
const devRoot = join(homedir(), ".bone", "dev");
const releasesDir = join(devRoot, "releases");
const currentLink = join(devRoot, "current");
const repositoryKey = createHash("sha256").update(repoRoot).digest("hex").slice(0, 16);
const hookDir = join(devRoot, "hooks", repositoryKey);
const hookConfigPath = join(hookDir, "config.json");
const retainedReleases = 5;

function fail(message) {
	throw new Error(message);
}

function run(command, args, options = {}) {
	const result = spawnSync(command, args, {
		cwd: options.cwd ?? repoRoot,
		encoding: "utf8",
		stdio: options.stdio ?? "inherit",
		env: options.env,
		timeout: options.timeout,
	});
	if (result.error) fail(`Could not run ${command}: ${result.error.message}`);
	if (result.status !== 0) fail(`${[command, ...args].join(" ")} exited with ${result.status ?? "an unknown status"}`);
	return result;
}

function runOutput(command, args, options = {}) {
	const result = run(command, args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
	return result.stdout.trim();
}

function resolveCommand(command, options = {}) {
	if (command.includes("/")) return existsSync(command) ? resolve(command) : undefined;
	for (const directory of (process.env.PATH ?? "").split(":")) {
		if (!directory) continue;
		const candidate = join(directory, command);
		if (options.skipNpmBin && candidate.replaceAll("\\", "/").includes("/node_modules/.bin/")) continue;
		if (existsSync(candidate)) return candidate;
	}
	return undefined;
}

function shellQuote(value) {
	return "'" + value.replaceAll("'", "'\"'\"'") + "'";
}

function usage() {
	console.error(`Usage:
  npm run dev:install [-- --skip-build]
  npm run dev:install-hook [-- --command <path-to-bone>]
  npm run dev:uninstall-hook`);
}

function parseArgs() {
	const [action, ...args] = process.argv.slice(2);
	if (action !== "install" && action !== "install-hook" && action !== "uninstall-hook") {
		usage();
		fail("A dev Bone action is required");
	}
	let skipBuild = false;
	let commandPath;
	for (let index = 0; index < args.length; index++) {
		const argument = args[index];
		if (argument === "--skip-build" && action === "install") {
			skipBuild = true;
			continue;
		}
		if (argument === "--command" && action === "install-hook" && args[index + 1]) {
			commandPath = args[++index];
			continue;
		}
		usage();
		fail(`Unknown option: ${argument}`);
	}
	return { action, skipBuild, commandPath };
}

function getPlatformTarget() {
	const target = `${process.platform}-${process.arch}`;
	if (!new Set(["darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64", "win32-x64", "win32-arm64"]).has(target)) {
		fail(`Bone dev install does not support ${target}`);
	}
	return target;
}

function getGitValue(args) {
	return runOutput("git", args);
}

function getNpmPath() {
	return resolveCommand("npm") ?? join(dirname(process.execPath), process.platform === "win32" ? "npm.cmd" : "npm");
}

function getBunPath() {
	const fallback = join(homedir(), ".bun", "bin", process.platform === "win32" ? "bun.exe" : "bun");
	return resolveCommand("bun") ?? (existsSync(fallback) ? fallback : undefined);
}

function buildTools() {
	const bunPath = getBunPath();
	if (!bunPath) fail("Bun is required to build the local Bone binary. Install Bun or add it to PATH.");
	const commandExtension = process.platform === "win32" ? ".cmd" : "";
	const tsgoPath = join(repoRoot, "node_modules", ".bin", `tsgo${commandExtension}`);
	if (!existsSync(tsgoPath)) fail("The TypeScript native compiler is missing. Run npm install --ignore-scripts first.");
	return {
		bunPath,
		tsgoPath,
		environment: { ...process.env, PATH: `${dirname(bunPath)}:${process.env.PATH ?? ""}` },
	};
}

function buildStandalone() {
	const { bunPath, tsgoPath, environment } = buildTools();
	const packageDirectories = [
		join(repoRoot, "packages", "tui"),
		join(repoRoot, "packages", "ai"),
		join(repoRoot, "packages", "agent"),
		codingAgentDir,
	];
	for (const directory of packageDirectories) {
		run(tsgoPath, ["-p", "tsconfig.build.json"], { cwd: directory, env: environment });
	}
	chmodSync(join(codingAgentDir, "dist", "cli.js"), 0o755);
	chmodSync(join(codingAgentDir, "dist", "rpc-entry.js"), 0o755);
	run(getNpmPath(), ["run", "copy-assets"], { cwd: codingAgentDir, env: environment });
	run(
		bunPath,
		[
			"build",
			"--compile",
			"./dist/bun/cli.js",
			"./src/utils/image-resize-worker.ts",
			"./src/core/local-embedding-worker.ts",
			"./src/core/local-embedding-setup-worker.ts",
			"--outfile",
			"dist/bone",
		],
		{ cwd: codingAgentDir, env: environment },
	);
	run(getNpmPath(), ["run", "copy-binary-assets"], { cwd: codingAgentDir, env: environment });
}

function writeJson(path, value) {
	writeFileSync(path, `${JSON.stringify(value, null, "\t")}\n`, { mode: 0o600 });
}

function readHookConfig() {
	if (!existsSync(hookConfigPath)) return undefined;
	try {
		const config = JSON.parse(readFileSync(hookConfigPath, "utf8"));
		if (config?.version !== 1 || config.repoRoot !== repoRoot || typeof config.commandPath !== "string") return undefined;
		return config;
	} catch {
		return undefined;
	}
}

function releaseName() {
	const version = JSON.parse(readFileSync(join(codingAgentDir, "package.json"), "utf8")).version;
	const commit = getGitValue(["rev-parse", "HEAD"]).slice(0, 12);
	const timestamp = new Date().toISOString().replaceAll(/[:.]/g, "-");
	return `${version}-${commit}-${timestamp}`;
}

function verifyRelease(releaseDirectory, platform) {
	const binaryName = process.platform === "win32" ? "bone.exe" : "bone";
	const binary = join(releaseDirectory, binaryName);
	if (!existsSync(binary)) fail(`Local Bone build is missing ${binaryName}`);
	if (process.platform !== "win32") chmodSync(binary, 0o755);
	run(process.execPath, [join(repoRoot, "scripts", "verify-semantic-native.mjs"), "--root", join(releaseDirectory, "native"), "--target", platform]);
	run(binary, ["--version"], { timeout: 30_000 });
}

function switchCurrentRelease(releaseDirectory) {
	mkdirSync(devRoot, { recursive: true, mode: 0o700 });
	const temporaryLink = join(devRoot, `.current-${process.boned}-${Date.now()}`);
	symlinkSync(relative(devRoot, releaseDirectory), temporaryLink, "dir");
	renameSync(temporaryLink, currentLink);
}

function ensureCommandShim(commandPath) {
	const expected = join(currentLink, process.platform === "win32" ? "bone.exe" : "bone");
	if (existsSync(commandPath)) {
		const metadata = lstatSync(commandPath);
		if (
			metadata.isSymbolicLink() &&
			resolve(dirname(commandPath), readlinkSync(commandPath)) === resolve(expected)
		) {
			return;
		}
		if (!metadata.isSymbolicLink() && !metadata.isFile()) {
			fail(`Bone command is not a file or symlink: ${commandPath}`);
		}
		rmSync(commandPath);
	}
	mkdirSync(dirname(commandPath), { recursive: true });
	symlinkSync(expected, commandPath);
}

function pruneReleases() {
	if (!existsSync(releasesDir)) return;
	const currentTarget = existsSync(currentLink) ? realpathSync(currentLink) : undefined;
	const releaseDirectories = readdirSync(releasesDir, { withFileTypes: true })
		.filter((entry) => entry.isDirectory())
		.map((entry) => ({ path: join(releasesDir, entry.name), modified: lstatSync(join(releasesDir, entry.name)).mtimeMs }))
		.sort((left, right) => right.modified - left.modified);
	for (const release of releaseDirectories.slice(retainedReleases)) {
		if (release.path !== currentTarget) rmSync(release.path, { recursive: true, force: true });
	}
}

function installRelease(skipBuild) {
	const platform = getPlatformTarget();
	if (!skipBuild) {
		console.log("Building current-platform Bone standalone binary...");
		buildStandalone();
	}

	const sourceDirectory = join(codingAgentDir, "dist");
	if (!existsSync(sourceDirectory)) fail("Bone build output is missing. Run npm run dev:install without --skip-build.");
	mkdirSync(releasesDir, { recursive: true, mode: 0o700 });
	const releaseDirectory = join(releasesDir, releaseName());
	const temporaryDirectory = `${releaseDirectory}.tmp-${process.boned}`;
	rmSync(temporaryDirectory, { recursive: true, force: true });
	cpSync(sourceDirectory, temporaryDirectory, { recursive: true });
	try {
		writeJson(join(temporaryDirectory, "bone-dev.json"), {
			version: 1,
			commit: getGitValue(["rev-parse", "HEAD"]),
			builtAt: new Date().toISOString(),
			platform,
			dirty: spawnSync("git", ["diff", "--quiet"], { cwd: repoRoot }).status !== 0,
		});
		verifyRelease(temporaryDirectory, platform);
		renameSync(temporaryDirectory, releaseDirectory);
		switchCurrentRelease(releaseDirectory);
		const hookConfig = readHookConfig();
		if (hookConfig) ensureCommandShim(resolve(hookConfig.commandPath));
		pruneReleases();
	} catch (error) {
		rmSync(temporaryDirectory, { recursive: true, force: true });
		throw error;
	}

	console.log(`Installed Bone dev binary: ${join(currentLink, process.platform === "win32" ? "bone.exe" : "bone")}`);
	return releaseDirectory;
}

function resolveBoneCommand(commandPath) {
	if (commandPath) return resolve(commandPath);
	const resolved = resolveCommand(process.platform === "win32" ? "bone.exe" : "bone", { skipNpmBin: true });
	if (!resolved) fail("Could not find bone on PATH. Re-run with --command <path-to-bone>.");
	return resolved;
}

function installCommandShim(commandPath) {
	const config = readHookConfig();
	if (config) {
		if (resolve(config.commandPath) !== commandPath) {
			fail(`This repository already manages ${config.commandPath}. Run npm run dev:uninstall-hook before changing the command path.`);
		}
		ensureCommandShim(commandPath);
		return config.commandBackup;
	}

	if (!existsSync(commandPath)) fail(`Bone command does not exist: ${commandPath}`);
	const metadata = lstatSync(commandPath);
	if (!metadata.isSymbolicLink() && !metadata.isFile()) fail(`Bone command is not a file or symlink: ${commandPath}`);
	try {
		mkdirSync(dirname(commandPath), { recursive: true });
	} catch {
		fail(`Bone command directory is not writable: ${dirname(commandPath)}`);
	}

	const backupDirectory = join(devRoot, "command-backups", repositoryKey);
	mkdirSync(backupDirectory, { recursive: true, mode: 0o700 });
	let commandBackup;
	if (metadata.isSymbolicLink()) {
		commandBackup = { type: "symlink", target: readlinkSync(commandPath) };
		rmSync(commandPath);
	} else {
		const backupPath = join(backupDirectory, `${basename(commandPath)}-${Date.now()}`);
		renameSync(commandPath, backupPath);
		commandBackup = { type: "file", path: backupPath };
	}
	ensureCommandShim(commandPath);
	return commandBackup;
}

function writeHook(path, source) {
	writeFileSync(path, source, { mode: 0o755 });
	chmodSync(path, 0o755);
}

function createInstallHookSource(hookName, originalHooksPath, logPath) {
	return `#!/bin/sh
if [ -x ${shellQuote(join(originalHooksPath, hookName))} ]; then
	${shellQuote(join(originalHooksPath, hookName))} "$@"
	status=$?
	if [ "$status" -ne 0 ]; then
		exit "$status"
	fi
fi
if [ "\${BONE_SKIP_LOCAL_INSTALL:-}" = "1" ]; then
	exit 0
fi
worktree_root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
if [ "$worktree_root" != ${shellQuote(repoRoot)} ]; then
	exit 0
fi
if ${shellQuote(process.execPath)} ${shellQuote(scriptPath)} install >> ${shellQuote(logPath)} 2>&1; then
	printf '%s\\n' "Bone dev install updated. Log: ${logPath}"
else
	printf '%s\\n' "Bone dev install failed; the previous binary is still active. Log: ${logPath}" >&2
fi
exit 0
`;
}

function getOriginalHooksPath() {
	const configured = spawnSync("git", ["config", "--get", "core.hooksPath"], {
		cwd: repoRoot,
		encoding: "utf8",
	});
	if (configured.status === 0 && configured.stdout.trim()) {
		const value = configured.stdout.trim();
		return { value, path: isAbsolute(value) ? value : resolve(repoRoot, value) };
	}
	const gitDirectory = getGitValue(["rev-parse", "--git-dir"]);
	return { value: undefined, path: resolve(repoRoot, gitDirectory, "hooks") };
}

function installHook(commandPath) {
	const existingConfig = readHookConfig();
	const { value: originalHooksPathValue, path: originalHooksPath } = existingConfig
		? { value: existingConfig.originalHooksPathValue, path: existingConfig.originalHooksPath }
		: getOriginalHooksPath();
	if (!existsSync(originalHooksPath)) fail(`Existing hooks path does not exist: ${originalHooksPath}`);
	const commandBackup = installCommandShim(commandPath);

	rmSync(hookDir, { recursive: true, force: true });
	mkdirSync(hookDir, { recursive: true, mode: 0o700 });
	for (const entry of readdirSync(originalHooksPath, { withFileTypes: true })) {
		if (!entry.isFile() || entry.name === "post-commit" || entry.name === "post-merge") continue;
		writeHook(join(hookDir, entry.name), `#!/bin/sh\nexec ${shellQuote(join(originalHooksPath, entry.name))} "$@"\n`);
	}
	const gitDirectory = resolve(repoRoot, getGitValue(["rev-parse", "--git-dir"]));
	const logPath = join(gitDirectory, "bone-dev-install.log");
	for (const hookName of ["post-commit", "post-merge"]) {
		writeHook(join(hookDir, hookName), createInstallHookSource(hookName, originalHooksPath, logPath));
	}

	writeJson(hookConfigPath, {
		version: 1,
		repoRoot,
		commandPath,
		commandBackup,
		originalHooksPathValue,
		originalHooksPath,
		hookDir,
	});
	run("git", ["config", "core.hooksPath", hookDir]);
	console.log(`Enabled Bone dev install after commits and merges for this clone.`);
	console.log(`bone now resolves through ${currentLink}.`);
}

function uninstallHook() {
	const config = readHookConfig();
	if (!config) fail("No Bone dev-install hook is configured for this repository.");
	const configuredHooksPath = spawnSync("git", ["config", "--get", "core.hooksPath"], {
		cwd: repoRoot,
		encoding: "utf8",
	}).stdout.trim();
	if (configuredHooksPath === config.hookDir) {
		if (config.originalHooksPathValue) run("git", ["config", "core.hooksPath", config.originalHooksPathValue]);
		else run("git", ["config", "--unset", "core.hooksPath"]);
	}

	if (existsSync(config.commandPath) && lstatSync(config.commandPath).isSymbolicLink()) {
		const target = readlinkSync(config.commandPath);
		const expected = join(currentLink, process.platform === "win32" ? "bone.exe" : "bone");
		if (target === expected) {
			rmSync(config.commandPath);
			if (config.commandBackup.type === "symlink") symlinkSync(config.commandBackup.target, config.commandPath);
			else renameSync(config.commandBackup.path, config.commandPath);
		}
	}
	rmSync(hookDir, { recursive: true, force: true });
	console.log("Disabled Bone dev install hook and restored the previous bone command when it was unchanged.");
}

try {
	const options = parseArgs();
	if (options.action === "install") installRelease(options.skipBuild);
	if (options.action === "install-hook") {
		installRelease(false);
		installHook(resolveBoneCommand(options.commandPath));
	}
	if (options.action === "uninstall-hook") uninstallHook();
} catch (error) {
	console.error(`Bone dev install failed: ${error instanceof Error ? error.message : String(error)}`);
	process.exitCode = 1;
}
