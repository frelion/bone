import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { loadEntriesFromFile, loadEntriesFromFileAsync, SessionManager } from "../src/core/session-manager.ts";

const tempDirs: string[] = [];

afterEach(() => {
	for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function createSessionFile(lines: string[]): string {
	const dir = mkdtempSync(join(tmpdir(), "bone-async-session-"));
	tempDirs.push(dir);
	const file = join(dir, "session.jsonl");
	writeFileSync(file, lines.join("\n"));
	return file;
}

describe("SessionManager.openAsync", () => {
	it("matches synchronous loading across UTF-8 chunk boundaries and a missing final newline", async () => {
		const header = { type: "session", version: 3, id: "session", timestamp: new Date().toISOString(), cwd: "/tmp" };
		const user = {
			type: "message",
			id: "user",
			parentId: null,
			timestamp: new Date().toISOString(),
			message: { role: "user", content: [{ type: "text", text: `你好${"x".repeat(1024 * 1024)}` }], timestamp: 1 },
		};
		const assistant = {
			type: "message",
			id: "assistant",
			parentId: "user",
			timestamp: new Date().toISOString(),
			message: {
				role: "assistant",
				content: [{ type: "text", text: "完成" }],
				provider: "test",
				model: "test",
				usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: {} },
				stopReason: "stop",
				timestamp: 2,
			},
		};
		const file = createSessionFile([JSON.stringify(header), JSON.stringify(user), JSON.stringify(assistant)]);

		expect(await loadEntriesFromFileAsync(file, { yieldIntervalMs: 0 })).toEqual(loadEntriesFromFile(file));
		const sync = SessionManager.open(file);
		const asyncManager = await SessionManager.openAsync(file, undefined, undefined, { yieldIntervalMs: 0 });
		expect(asyncManager.getEntries()).toEqual(sync.getEntries());
		expect(asyncManager.getLeafId()).toBe(sync.getLeafId());
		expect(asyncManager.buildSessionContext()).toEqual(sync.buildSessionContext());
	});

	it("honors cancellation before parsing completes", async () => {
		const header = { type: "session", version: 3, id: "session", timestamp: new Date().toISOString(), cwd: "/tmp" };
		const lines = [JSON.stringify(header)];
		for (let index = 0; index < 500; index++) {
			lines.push(
				JSON.stringify({
					type: "custom",
					id: `entry-${index}`,
					parentId: index === 0 ? null : `entry-${index - 1}`,
					timestamp: new Date().toISOString(),
					customType: "test",
					data: "x".repeat(4096),
				}),
			);
		}
		const file = createSessionFile(lines);
		const controller = new AbortController();
		controller.abort();
		await expect(loadEntriesFromFileAsync(file, { signal: controller.signal })).rejects.toMatchObject({
			name: "AbortError",
		});
	});
});
