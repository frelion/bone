#!/usr/bin/env bun

import { chmodSync, copyFileSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

function usage() {
	console.error(
		"Usage: bun scripts/verify-opentui-standalone.mjs --binary <path> [--native-library <path>] [--skip-run]",
	);
}

const args = process.argv.slice(2);
let binaryArgument;
let nativeLibraryArgument;
let skipRun = false;
for (let index = 0; index < args.length; index++) {
	const argument = args[index];
	if (argument === "--binary" && args[index + 1]) {
		binaryArgument = args[++index];
		continue;
	}
	if (argument === "--native-library" && args[index + 1]) {
		nativeLibraryArgument = args[++index];
		continue;
	}
	if (argument === "--skip-run") {
		skipRun = true;
		continue;
	}
	usage();
	process.exit(2);
}
if (!binaryArgument || (skipRun && !nativeLibraryArgument)) {
	usage();
	process.exit(2);
}

const sourceBinary = resolve(binaryArgument);
const temporaryDirectory = mkdtempSync(join(tmpdir(), "bone-opentui-standalone-"));
const isolatedBinary = join(temporaryDirectory, basename(sourceBinary));

function run(arguments_, environment = {}) {
	const result = spawnSync(isolatedBinary, arguments_, {
		cwd: temporaryDirectory,
		encoding: "utf8",
		env: {
			...process.env,
			BONE_VERIFY_OPENTUI_NATIVE: undefined,
			...environment,
			OTUI_ASSET_ROOT: undefined,
		},
	});
	if (result.error) throw result.error;
	if (result.status !== 0) {
		throw new Error(
			`${isolatedBinary} ${arguments_.join(" ")} failed with status ${result.status}\n${result.stderr || result.stdout}`,
		);
	}
	return result.stdout;
}

try {
	if (nativeLibraryArgument) {
		const executable = readFileSync(sourceBinary);
		const nativeLibrary = readFileSync(resolve(nativeLibraryArgument));
		if (executable.indexOf(nativeLibrary) === -1) {
			throw new Error(`OpenTUI native library is not embedded in ${sourceBinary}`);
		}
		console.log(`Verified embedded OpenTUI native library bytes: ${sourceBinary}`);
	}
	if (!skipRun) {
		copyFileSync(sourceBinary, isolatedBinary);
		if (process.platform !== "win32") chmodSync(isolatedBinary, 0o755);

		const probeOutput = run([], { BONE_VERIFY_OPENTUI_NATIVE: "1" });
		if (!probeOutput.includes("BONE_OPENTUI_NATIVE_OK")) {
			throw new Error(`OpenTUI native probe did not report success:\n${probeOutput}`);
		}
		run(["--help"]);
		console.log(`Verified embedded OpenTUI native library and standalone startup: ${sourceBinary}`);
	}
} finally {
	rmSync(temporaryDirectory, { recursive: true, force: true });
}
