import { homedir } from "node:os";
import { join } from "node:path";
import { normalizePath } from "./paths.ts";

export function getAgentDir(): string {
	const configured = process.env.BONE_CODING_AGENT_DIR;
	return configured ? normalizePath(configured) : join(homedir(), ".bone", "agent");
}

export function getSessionsDir(): string {
	return join(getAgentDir(), "sessions");
}
