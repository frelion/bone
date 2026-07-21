import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { utimes } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { SessionManager } from "../src/core/session-manager.ts";

const tempDirs: string[] = [];

afterEach(() => {
	for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe("SessionManager.listPage", () => {
	it("parses only the requested mtime-ordered page", async () => {
		const dir = mkdtempSync(join(tmpdir(), "bone-session-page-"));
		tempDirs.push(dir);
		for (let index = 0; index < 5; index++) {
			const timestamp = new Date(Date.now() + index * 1000).toISOString();
			const header = { type: "session", version: 3, id: `session-${index}`, timestamp, cwd: dir };
			const message = {
				type: "message",
				id: `message-${index}`,
				parentId: null,
				timestamp,
				message: { role: "user", content: [{ type: "text", text: `message ${index}` }], timestamp: index },
			};
			const file = join(dir, `${index}.jsonl`);
			writeFileSync(file, `${JSON.stringify(header)}\n${JSON.stringify(message)}\n`);
			const modified = new Date(Date.now() + index * 1000);
			await utimes(file, modified, modified);
		}

		const first = await SessionManager.listPage(dir, dir, 0, 2);
		expect(first.total).toBe(5);
		expect(first.hasMore).toBe(true);
		expect(first.sessions.map((session) => session.id)).toEqual(["session-4", "session-3"]);

		const second = await SessionManager.listPage(dir, dir, 2, 2);
		expect(second.sessions.map((session) => session.id)).toEqual(["session-2", "session-1"]);
	});
});
