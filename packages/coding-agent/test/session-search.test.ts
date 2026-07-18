import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import type { LocalEmbeddingEngine } from "../src/core/local-embedding.ts";
import type { SessionInfo } from "../src/core/session-manager.ts";
import {
	extractSessionSearchDocuments,
	getSessionSearchDatabasePath,
	SessionSearchService,
} from "../src/core/session-search.ts";
import { normalizeSearchTerms } from "../src/core/session-search-normalizer.ts";

const temporaryDirectories: string[] = [];

async function createTemporaryDirectory(): Promise<string> {
	const directory = await mkdtemp(join(tmpdir(), "bone-session-search-"));
	temporaryDirectories.push(directory);
	return directory;
}

function makeSession(path: string): SessionInfo {
	return {
		path,
		id: "session-a",
		cwd: "/workspace/bone",
		name: "Session sidebar lifecycle",
		created: new Date("2026-07-18T10:00:00.000Z"),
		modified: new Date("2026-07-18T10:05:00.000Z"),
		messageCount: 2,
		firstMessage: "切换会话后输出不见了",
		allMessagesText: "切换会话后输出不见了 修复 session-sidebar.ts runtime rebind",
		lastMessage: "修复 session-sidebar.ts runtime rebind",
		lastMessageRole: "assistant",
	};
}

function vector(value: number): Float32Array {
	const result = new Float32Array(384);
	result[value] = 1;
	return result;
}

class FakeEmbeddingEngine implements LocalEmbeddingEngine {
	documentCalls = 0;
	queryCalls = 0;

	async embedQuery(query: string): Promise<Float32Array> {
		this.queryCalls++;
		return vector(query.includes("释放") ? 1 : 0);
	}

	async embedDocuments(documents: readonly string[]): Promise<Float32Array[]> {
		this.documentCalls++;
		return documents.map((document) => vector(document.includes("关闭后台模型") ? 1 : 0));
	}

	async dispose(): Promise<void> {}
}

afterEach(async () => {
	await Promise.all(
		temporaryDirectories.splice(0).map((directory) => rm(directory, { recursive: true, force: true })),
	);
});

describe("Session search", () => {
	it("normalizes CJK, identifiers, and paths into deterministic terms", () => {
		const terms = normalizeSearchTerms("修 SessionSidebar 的 session-sidebar.ts 与 apiKey");
		expect(terms).toContain("sessionsidebar");
		expect(terms).toContain("session");
		expect(terms).toContain("sidebar");
		expect(terms).toContain("apikey");
		expect(terms).toContain("api");
		expect(terms).toContain("key");
		expect(terms).toContain("session-sidebar.ts");
		expect(terms).toContain("session sidebar");
	});

	it("extracts user tasks, final responses, and safe references from JSONL", async () => {
		const directory = await createTemporaryDirectory();
		const sessionPath = join(directory, "session.jsonl");
		await writeFile(
			sessionPath,
			[
				JSON.stringify({
					type: "session",
					id: "session-a",
					timestamp: "2026-07-18T10:00:00.000Z",
					cwd: "/workspace/bone",
				}),
				JSON.stringify({
					type: "message",
					id: "user-1",
					parentId: null,
					timestamp: "2026-07-18T10:01:00.000Z",
					message: {
						role: "user",
						content: "切换会话后输出不见了，检查 packages/coding-agent/src/session-sidebar.ts",
					},
				}),
				JSON.stringify({
					type: "message",
					id: "assistant-1",
					parentId: "user-1",
					timestamp: "2026-07-18T10:02:00.000Z",
					message: { role: "assistant", content: "修复 runtime rebind，并运行 npm test", stopReason: "stop" },
				}),
			].join("\n"),
		);

		const documents = await extractSessionSearchDocuments(makeSession(sessionPath));
		expect(documents.map((document) => document.kind)).toEqual(["title", "user", "reference"]);
		expect(documents[1]?.displayText).toContain("切换会话后输出不见了");
		expect(documents[2]?.displayText).toContain("session-sidebar.ts");
	});

	it("uses LanceDB FTS for task and code-reference search and purges deleted sessions", async () => {
		const directory = await createTemporaryDirectory();
		const sessionPath = join(directory, "session.jsonl");
		await writeFile(
			sessionPath,
			[
				JSON.stringify({ type: "session", id: "session-a", timestamp: "2026-07-18T10:00:00.000Z", cwd: directory }),
				JSON.stringify({
					type: "message",
					id: "u",
					parentId: null,
					timestamp: "2026-07-18T10:01:00.000Z",
					message: { role: "user", content: "切换会话后输出不见了" },
				}),
				JSON.stringify({
					type: "message",
					id: "a",
					parentId: "u",
					timestamp: "2026-07-18T10:02:00.000Z",
					message: { role: "assistant", content: "修复 session-sidebar.ts runtime rebind", stopReason: "stop" },
				}),
			].join("\n"),
		);
		const session = makeSession(sessionPath);
		const service = new SessionSearchService({ agentDir: join(directory, "agent"), cwd: directory });

		expect((await service.search("切换 会话 输出", [session])).map((result) => result.sessionPath)).toContain(
			sessionPath,
		);
		expect((await service.search("session-sidebar.ts", [session])).map((result) => result.sessionPath)).toContain(
			sessionPath,
		);

		await service.remove(sessionPath);
		service.invalidate();
		expect(await service.search("session-sidebar.ts", [])).toEqual([]);
		await service.dispose();
	});

	it("uses LanceDB vector search for semantic recall and retains unchanged embeddings", async () => {
		const directory = await createTemporaryDirectory();
		const firstPath = join(directory, "first.jsonl");
		const secondPath = join(directory, "second.jsonl");
		await writeFile(
			firstPath,
			[
				JSON.stringify({ type: "session", id: "first", timestamp: "2026-07-18T10:00:00.000Z", cwd: directory }),
				JSON.stringify({
					type: "message",
					id: "first-user",
					parentId: null,
					timestamp: "2026-07-18T10:01:00.000Z",
					message: { role: "user", content: "修复聊天侧栏的显示问题" },
				}),
				JSON.stringify({
					type: "message",
					id: "first-assistant",
					parentId: "first-user",
					timestamp: "2026-07-18T10:02:00.000Z",
					message: { role: "assistant", content: "完成会话列表排版", stopReason: "stop" },
				}),
			].join("\n"),
		);
		await writeFile(
			secondPath,
			[
				JSON.stringify({ type: "session", id: "second", timestamp: "2026-07-18T11:00:00.000Z", cwd: directory }),
				JSON.stringify({
					type: "message",
					id: "second-user",
					parentId: null,
					timestamp: "2026-07-18T11:01:00.000Z",
					message: { role: "user", content: "清理长时间未使用的本地模型" },
				}),
				JSON.stringify({
					type: "message",
					id: "second-assistant",
					parentId: "second-user",
					timestamp: "2026-07-18T11:02:00.000Z",
					message: { role: "assistant", content: "关闭后台模型并释放 CPU 内存", stopReason: "stop" },
				}),
			].join("\n"),
		);
		const embeddingEngine = new FakeEmbeddingEngine();
		const service = new SessionSearchService({ agentDir: join(directory, "agent"), cwd: directory, embeddingEngine });
		const first = makeSession(firstPath);
		const second = {
			...makeSession(secondPath),
			id: "session-b",
			name: "Local model cleanup",
			firstMessage: "清理长时间未使用的本地模型",
		};

		expect((await service.searchSemantic("释放本地资源", [first, second]))[0]?.sessionPath).toBe(secondPath);
		expect(embeddingEngine.documentCalls).toBe(1);
		service.invalidate();
		await service.searchSemantic("释放本地资源", [first, second]);
		expect(embeddingEngine.documentCalls).toBe(1);
		await service.dispose();
	});

	it("keeps workspaces isolated and overlays a live unpersisted conversation", async () => {
		const directory = await createTemporaryDirectory();
		const agentDir = join(directory, "agent");
		const workspaceA = join(directory, "workspace-a");
		const workspaceB = join(directory, "workspace-b");
		expect(getSessionSearchDatabasePath(agentDir, workspaceA)).not.toBe(
			getSessionSearchDatabasePath(agentDir, workspaceB),
		);
		const transient = {
			...makeSession(join(directory, "not-yet-written.jsonl")),
			firstMessage: "后台 runner 仍在执行索引",
			lastMessage: "等待后台 runner 完成",
		};
		const service = new SessionSearchService({ agentDir, cwd: workspaceA });

		expect((await service.search("后台 runner", [transient])).map((result) => result.sessionPath)).toEqual([
			transient.path,
		]);
		await service.dispose();
	});
});
