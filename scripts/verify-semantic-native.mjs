#!/usr/bin/env bun

import { existsSync, statSync } from "node:fs";
import { resolve } from "node:path";

const requiredFiles = {
	"darwin-arm64": ["libcrispembed.0.dylib", "libggml.0.dylib", "libggml-cpu.0.dylib", "libggml-base.0.dylib"],
	"darwin-x64": ["libcrispembed.0.dylib", "libggml.0.dylib", "libggml-cpu.0.dylib", "libggml-base.0.dylib"],
	"linux-x64": ["libcrispembed.so.0", "libggml.so.0", "libggml-cpu.so.0", "libggml-base.so.0"],
	"linux-arm64": ["libcrispembed.so.0", "libggml.so.0", "libggml-cpu.so.0", "libggml-base.so.0"],
	"win32-x64": ["crispembed.dll", "ggml.dll", "ggml-cpu.dll", "ggml-base.dll"],
	"win32-arm64": ["crispembed.dll", "ggml.dll", "ggml-cpu.dll", "ggml-base.dll"],
};

function currentTarget() {
	const target = `${process.platform}-${process.arch}`;
	if (!(target in requiredFiles)) throw new Error(`No semantic native layout is defined for ${target}`);
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
	throw new Error("Usage: bun scripts/verify-semantic-native.mjs --root <native-directory> [--target <platform>]");
	}
	if (!root) throw new Error("--root is required");
	target ??= currentTarget();
	if (!(target in requiredFiles)) throw new Error(`Unsupported semantic native target: ${target}`);
	return { root, target };
}

const { root, target } = parseOptions();
const targetDirectory = resolve(root, target);
const missing = requiredFiles[target].filter((file) => !existsSync(resolve(targetDirectory, file)));
if (missing.length > 0) {
	throw new Error(`Incomplete ${target} semantic native runtime in ${targetDirectory}: missing ${missing.join(", ")}`);
}

for (const filename of requiredFiles[target]) {
	const library = resolve(targetDirectory, filename);
	if (!statSync(library).isFile()) throw new Error(`Semantic Bun FFI library is invalid: ${library}`);
}

console.log(`Verified ${target} semantic Bun FFI libraries: ${targetDirectory}`);
