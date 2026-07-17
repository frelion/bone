import { randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { resolvePath } from "../utils/paths.ts";

const STATE_FILE_NAME = "conversation-state.json";
const STATE_VERSION = 1;

interface ConversationStateFile {
	version: typeof STATE_VERSION;
	lastActiveByWorkspace: Record<string, string>;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function getStatePath(agentDir: string): string {
	return join(resolvePath(agentDir), STATE_FILE_NAME);
}

function getWorkspaceKey(cwd: string, sessionDir: string | undefined, agentDir: string): string {
	const resolvedCwd = resolvePath(cwd);
	const resolvedAgentDir = resolvePath(agentDir);
	const defaultSessionDir = join(
		resolvedAgentDir,
		"sessions",
		`--${resolvedCwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`,
	);
	const resolvedSessionDir = resolvePath(sessionDir ?? defaultSessionDir);
	return `${resolvedCwd}\u0000${resolvedSessionDir}`;
}

function readState(agentDir: string): ConversationStateFile {
	const statePath = getStatePath(agentDir);
	if (!existsSync(statePath)) {
		return { version: STATE_VERSION, lastActiveByWorkspace: {} };
	}

	try {
		const parsed: unknown = JSON.parse(readFileSync(statePath, "utf8"));
		if (!isRecord(parsed) || parsed.version !== STATE_VERSION || !isRecord(parsed.lastActiveByWorkspace)) {
			return { version: STATE_VERSION, lastActiveByWorkspace: {} };
		}

		const lastActiveByWorkspace = Object.fromEntries(
			Object.entries(parsed.lastActiveByWorkspace).filter(
				(entry): entry is [string, string] => typeof entry[1] === "string",
			),
		);
		return { version: STATE_VERSION, lastActiveByWorkspace };
	} catch {
		return { version: STATE_VERSION, lastActiveByWorkspace: {} };
	}
}

/** Gets the persisted foreground conversation for a workspace/session directory pair. */
export function getLastActiveConversation(
	cwd: string,
	sessionDir: string | undefined,
	agentDir: string,
): string | undefined {
	const state = readState(agentDir);
	const sessionPath = state.lastActiveByWorkspace[getWorkspaceKey(cwd, sessionDir, agentDir)];
	return sessionPath ? resolvePath(sessionPath) : undefined;
}

/** Persists the current foreground conversation so the next Bone launch restores it. */
export function rememberLastActiveConversation(
	cwd: string,
	sessionDir: string | undefined,
	sessionPath: string,
	agentDir: string,
): void {
	const statePath = getStatePath(agentDir);
	const state = readState(agentDir);
	state.lastActiveByWorkspace[getWorkspaceKey(cwd, sessionDir, agentDir)] = resolvePath(sessionPath);

	mkdirSync(dirname(statePath), { recursive: true });
	const temporaryPath = `${statePath}.${randomUUID()}.tmp`;
	try {
		writeFileSync(temporaryPath, `${JSON.stringify(state, null, "\t")}\n`, { encoding: "utf8", mode: 0o600 });
		renameSync(temporaryPath, statePath);
	} catch {
		try {
			if (existsSync(temporaryPath)) {
				unlinkSync(temporaryPath);
			}
		} catch {
			// The persisted location is a convenience; a write failure must not interrupt a conversation.
		}
	}
}
