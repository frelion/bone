#!/usr/bin/env bun
import "./require-bun.ts";
import { verifyOpenTUINativeRuntime } from "@frelion/bone-tui";
import { APP_NAME } from "../config.ts";

process.title = APP_NAME;
process.emitWarning = (() => {}) as typeof process.emitWarning;

import { restoreSandboxEnv } from "./restore-sandbox-env.ts";

restoreSandboxEnv();

if (process.env.BONE_VERIFY_OPENTUI_NATIVE === "1") {
	verifyOpenTUINativeRuntime();
	console.log("BONE_OPENTUI_NATIVE_OK");
	process.exit(0);
}

await import("./register-bedrock.ts");
await import("../cli.ts");
