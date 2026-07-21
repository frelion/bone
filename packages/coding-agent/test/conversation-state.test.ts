import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
	forgetLastActiveConversation,
	getLastActiveConversation,
	rememberLastActiveConversation,
} from "../src/core/conversation-state.ts";

describe("conversation state", () => {
	const tempDirs: string[] = [];

	afterEach(() => {
		for (const tempDir of tempDirs.splice(0)) {
			if (existsSync(tempDir)) {
				rmSync(tempDir, { recursive: true, force: true });
			}
		}
	});

	it("remembers the last active conversation per workspace and session directory", () => {
		const root = join(tmpdir(), `bone-conversation-state-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(root);
		const agentDir = join(root, "agent");
		const workspaceA = join(root, "workspace-a");
		const workspaceB = join(root, "workspace-b");
		const sharedSessions = join(root, "sessions");
		mkdirSync(sharedSessions, { recursive: true });

		const sessionA = join(sharedSessions, "a.jsonl");
		const sessionB = join(sharedSessions, "b.jsonl");
		rememberLastActiveConversation(workspaceA, sharedSessions, sessionA, agentDir);
		rememberLastActiveConversation(workspaceB, sharedSessions, sessionB, agentDir);

		expect(getLastActiveConversation(workspaceA, sharedSessions, agentDir)).toBe(sessionA);
		expect(getLastActiveConversation(workspaceB, sharedSessions, agentDir)).toBe(sessionB);
	});

	it("treats corrupted persisted state as unavailable", () => {
		const root = join(tmpdir(), `bone-conversation-state-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(root);
		const agentDir = join(root, "agent");
		mkdirSync(agentDir, { recursive: true });
		writeFileSync(join(agentDir, "conversation-state.json"), "not json");

		expect(getLastActiveConversation(join(root, "workspace"), undefined, agentDir)).toBeUndefined();
	});

	it("forgets every workspace pointer to an archived conversation", () => {
		const root = join(tmpdir(), `bone-conversation-state-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(root);
		const agentDir = join(root, "agent");
		const sharedSessions = join(root, "sessions");
		const workspace = join(root, "workspace");
		const session = join(sharedSessions, "archived.jsonl");
		mkdirSync(sharedSessions, { recursive: true });

		rememberLastActiveConversation(workspace, sharedSessions, session, agentDir);
		forgetLastActiveConversation(session, agentDir);

		expect(getLastActiveConversation(workspace, sharedSessions, agentDir)).toBeUndefined();
	});
});
