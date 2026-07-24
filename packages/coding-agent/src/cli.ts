#!/usr/bin/env bun
import "./bun/require-bun.ts";
/**
 * CLI entry point for the refactored coding agent.
 * Uses main.ts with AgentSession and new mode modules.
 *
 * Test with: bun src/cli.ts [args...]
 */
import { APP_NAME } from "./config.ts";
import { main } from "./main.ts";

process.title = APP_NAME;
process.env.BONE_CODING_AGENT = "true";
process.emitWarning = (() => {}) as typeof process.emitWarning;

main(process.argv.slice(2));
