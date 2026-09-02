import { homedir } from "node:os";
import { join } from "node:path";

export function getDefaultForgeAgentDir(): string {
	const configured = process.env.BONE_CODING_AGENT_DIR;
	if (!configured) return join(homedir(), ".bone", "agent");
	if (configured === "~") return homedir();
	if (configured.startsWith("~/") || (process.platform === "win32" && configured.startsWith("~\\"))) {
		return join(homedir(), configured.slice(2));
	}
	return configured;
}
