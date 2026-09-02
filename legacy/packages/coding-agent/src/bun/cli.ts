#!/usr/bin/env bun
import "./require-bun.ts";
import { registerBunOAuthModules } from "@frelion/bone-ai/bun";
import { verifyOpenTUINativeRuntime } from "@frelion/bone-tui";
import { APP_NAME } from "../config.ts";

registerBunOAuthModules();

process.title = APP_NAME;
process.emitWarning = (() => {}) as typeof process.emitWarning;

import { restoreSandboxEnv } from "./restore-sandbox-env.ts";

restoreSandboxEnv();

if (process.env.BONE_VERIFY_STANDALONE_RUNTIME === "1") {
	const [{ loadClipboardNative, loadPhoton }, { getLocalEmbeddingNativeLibraryPath }, { BunFfiEmbeddingLibrary }] =
		await Promise.all([
			import("@frelion/bone-images"),
			import("@frelion/bone-memory"),
			import("@frelion/bone-memory/local-embedding-ffi"),
		]);
	verifyOpenTUINativeRuntime();
	const photon = await loadPhoton();
	if (!photon) throw new Error("Embedded Photon runtime could not be loaded");
	const clipboard = loadClipboardNative();
	if (!clipboard) throw new Error("Embedded clipboard runtime could not be loaded");
	const embeddingLibrary = new BunFfiEmbeddingLibrary(getLocalEmbeddingNativeLibraryPath());
	embeddingLibrary.close();
	console.log("BONE_STANDALONE_RUNTIME_OK");
	process.exit(0);
}

if (process.env.BONE_VERIFY_OPENTUI_NATIVE === "1") {
	verifyOpenTUINativeRuntime();
	console.log("BONE_OPENTUI_NATIVE_OK");
	process.exit(0);
}

await import("./register-bedrock.ts");
await import("../cli.ts");
