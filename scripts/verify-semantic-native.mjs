#!/usr/bin/env node

import { existsSync, statSync } from "node:fs";
import { resolve } from "node:path";

const requiredFiles = {
	"darwin-arm64": ["bone-embed", "libcrispembed.0.dylib", "libggml.0.dylib", "libggml-cpu.0.dylib", "libggml-base.0.dylib"],
	"darwin-x64": ["bone-embed", "libcrispembed.0.dylib", "libggml.0.dylib", "libggml-cpu.0.dylib", "libggml-base.0.dylib"],
	"linux-x64": ["bone-embed", "libcrispembed.so.0", "libggml.so.0", "libggml-cpu.so.0", "libggml-base.so.0"],
	"linux-arm64": ["bone-embed", "libcrispembed.so.0", "libggml.so.0", "libggml-cpu.so.0", "libggml-base.so.0"],
	"win32-x64": ["bone-embed.exe", "crispembed.dll", "ggml.dll", "ggml-cpu.dll", "ggml-base.dll"],
	"win32-arm64": ["bone-embed.exe", "crispembed.dll", "ggml.dll", "ggml-cpu.dll", "ggml-base.dll"],
};

function currentTarget() {
	const target = `${process.platform}-${process.arch}`;
	if (!(target in requiredFiles)) throw new Error(`No semantic sidecar layout is defined for ${target}`);
	return target;
}

function parseOptions() {
	let root;
	let target;
	const args = process.argv.slice(2);
	for (let index = 0; index < args.length; index++) {
		const arg = args[index];
		if (arg === "--root" && args[index + 1]) {
			root = resolve(args[++index]);
			continue;
		}
		if (arg === "--target" && args[index + 1]) {
			target = args[++index];
			continue;
		}
		throw new Error("Usage: node scripts/verify-semantic-native.mjs --root <native-directory> [--target <platform>]");
	}
	if (!root) throw new Error("--root is required");
	target ??= currentTarget();
	if (!(target in requiredFiles)) throw new Error(`Unsupported semantic sidecar target: ${target}`);
	return { root, target };
}

const { root, target } = parseOptions();
const targetDirectory = resolve(root, target);
const missing = requiredFiles[target].filter((file) => !existsSync(resolve(targetDirectory, file)));
if (missing.length > 0) {
	throw new Error(`Incomplete ${target} semantic sidecar in ${targetDirectory}: missing ${missing.join(", ")}`);
}

const executable = resolve(targetDirectory, requiredFiles[target][0]);
if (process.platform !== "win32" && (statSync(executable).mode & 0o111) === 0) {
	throw new Error(`Semantic sidecar is not executable: ${executable}`);
}

console.log(`Verified ${target} semantic sidecar: ${targetDirectory}`);
