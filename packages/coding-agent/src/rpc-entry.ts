#!/usr/bin/env bun
import "./bun/require-bun.ts";
import { APP_NAME } from "./config.ts";
import { main } from "./main.ts";

process.title = `${APP_NAME}-rpc`;
process.env.BONE_CODING_AGENT = "true";
process.emitWarning = (() => {}) as typeof process.emitWarning;

main(["--mode", "rpc", ...process.argv.slice(2)]);
