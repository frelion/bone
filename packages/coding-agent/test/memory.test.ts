import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { getLocalEmbeddingAvailability, type LocalEmbeddingEngine } from "../src/core/local-embedding.ts";
import { extractMemoryItems, getMemoryDatabasePath, MemoryRuntime } from "../src/core/memory.ts";
import { type SessionInfo, SessionManager } from "../src/core/session-manager.ts";
import { normalizeSearchTerms } from "../src/core/session-search-normalizer.ts";
import { assistantMsg } from "./utilities.ts";

const temporaryDirectories: string[] = [];

async function createTemporaryDirectory(): Promise<string> {
	const directory = await mkdtemp(join(tmpdir(), "bone-memory-"));
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
	prepareCalls = 0;
	documentCalls = 0;
	queryCalls = 0;

	async prepare(): Promise<void> {
		this.prepareCalls++;
	}

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

class ZeroEmbeddingEngine extends FakeEmbeddingEngine {
	override async embedDocuments(documents: readonly string[]): Promise<Float32Array[]> {
		this.documentCalls++;
		return documents.map(() => new Float32Array(384));
	}
}

afterEach(async () => {
	await Promise.all(
		temporaryDirectories.splice(0).map((directory) => rm(directory, { recursive: true, force: true })),
	);
});

describe("Memory runtime", () => {
	it("normalizes CJK, identifiers, and paths deterministically", () => {
		const terms = normalizeSearchTerms("修 SessionSidebar 的 session-sidebar.ts 与 apiKey");
		expect(terms).toContain("sessionsidebar");
		expect(terms).toContain("session");
		expect(terms).toContain("sidebar");
		expect(terms).toContain("apikey");
		expect(terms).toContain("api");
		expect(terms).toContain("key");
		expect(terms).toContain("session-sidebar.ts");
	});

	it("materializes exchanges instead of mirroring every JSONL entry", async () => {
		const directory = await createTemporaryDirectory();
		const sessionPath = join(directory, "session.jsonl");
		await writeFile(
			sessionPath,
			[
				JSON.stringify({ type: "session", id: "session-a", timestamp: "2026-07-18T10:00:00.000Z", cwd: directory }),
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
					id: "tool-call",
					parentId: "user-1",
					timestamp: "2026-07-18T10:02:00.000Z",
					message: {
						role: "assistant",
						content: [{ type: "toolCall", id: "x", name: "read", arguments: {} }],
						stopReason: "stop",
					},
				}),
				JSON.stringify({
					type: "message",
					id: "assistant-1",
					parentId: "tool-call",
					timestamp: "2026-07-18T10:03:00.000Z",
					message: { role: "assistant", content: "修复 runtime rebind，并运行 npm test", stopReason: "stop" },
				}),
			].join("\n"),
		);

		const items = await extractMemoryItems(makeSession(sessionPath));
		expect(items.filter((item) => item.kind === "conversation-exchange")).toHaveLength(1);
		expect(items.find((item) => item.kind === "conversation-exchange")?.semanticText).toContain(
			"Final result: 修复 runtime rebind",
		);
		expect(items.some((item) => item.kind === "file-reference")).toBe(true);
		expect(items.some((item) => item.kind === "command-reference")).toBe(true);
	});

	it("reports the first buffered user entry only when the JSONL file is actually flushed", async () => {
		const directory = await createTemporaryDirectory();
		const manager = SessionManager.create(directory, directory);
		const user = manager.appendMessageWithPersistence({
			role: "user",
			content: "remember this task",
			timestamp: Date.now(),
		});
		expect(user.persistedEntries).toEqual([]);
		const assistant = manager.appendMessageWithPersistence(assistantMsg("final response"));
		expect(
			assistant.persistedEntries.filter((entry) => entry.type === "message").map((entry) => entry.message.role),
		).toEqual(["user", "assistant"]);
	});

	it("reconciles JSONL at startup, then serves lexical reads without a second scan", async () => {
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
		const runtime = new MemoryRuntime({
			agentDir: join(directory, "agent"),
			cwd: directory,
			embeddingEngine: new FakeEmbeddingEngine(),
		});
		await runtime.start([session]);

		const cjkResults = await runtime.search("切换 会话 输出", [session]);
		const pathResults = await runtime.search("session-sidebar.ts", [session]);
		expect(pathResults.map((result) => result.sessionPath)).toContain(sessionPath);
		expect(cjkResults.map((result) => result.sessionPath)).toContain(sessionPath);

		await runtime.removeSession(sessionPath);
		expect(await runtime.search("session-sidebar.ts", [])).toEqual([]);
		await runtime.dispose();
	});

	it("replaces historical title terms when a conversation is renamed", async () => {
		const directory = await createTemporaryDirectory();
		const sessionPath = join(directory, "renamed.jsonl");
		await writeFile(
			sessionPath,
			[
				JSON.stringify({ type: "session", id: "renamed", timestamp: "2026-07-18T10:00:00.000Z", cwd: directory }),
				JSON.stringify({
					type: "session_info",
					id: "title-old",
					timestamp: "2026-07-18T10:01:00.000Z",
					name: "Old title",
				}),
				JSON.stringify({
					type: "message",
					id: "user-1",
					parentId: "title-old",
					timestamp: "2026-07-18T10:02:00.000Z",
					message: { role: "user", content: "Implement the sidebar search flow" },
				}),
				JSON.stringify({
					type: "message",
					id: "assistant-1",
					parentId: "user-1",
					timestamp: "2026-07-18T10:03:00.000Z",
					message: { role: "assistant", content: "Implemented the interaction", stopReason: "stop" },
				}),
			].join("\n"),
		);
		const session = { ...makeSession(sessionPath), name: "Old title" };
		const runtime = new MemoryRuntime({
			agentDir: join(directory, "agent"),
			cwd: directory,
			embeddingEngine: new FakeEmbeddingEngine(),
		});
		await runtime.start([session]);

		expect((await runtime.search("old title", [session])).map((result) => result.sessionPath)).toEqual([sessionPath]);
		await runtime.recordTitle({ path: sessionPath, id: session.id }, "New title");
		expect(await runtime.search("old title", [session])).toEqual([]);
		expect((await runtime.search("new title", [session])).map((result) => result.sessionPath)).toEqual([sessionPath]);

		await runtime.dispose();
	});

	it("uses only the latest saved title while rebuilding a conversation", async () => {
		const directory = await createTemporaryDirectory();
		const sessionPath = join(directory, "renamed-during-rebuild.jsonl");
		await writeFile(
			sessionPath,
			[
				JSON.stringify({ type: "session", id: "renamed", timestamp: "2026-07-18T10:00:00.000Z", cwd: directory }),
				JSON.stringify({
					type: "session_info",
					id: "title-old",
					timestamp: "2026-07-18T10:01:00.000Z",
					name: "Old title",
				}),
				JSON.stringify({
					type: "message",
					id: "user-1",
					parentId: "title-old",
					timestamp: "2026-07-18T10:02:00.000Z",
					message: { role: "user", content: "Implement the sidebar search flow" },
				}),
				JSON.stringify({
					type: "session_info",
					id: "title-new",
					parentId: "user-1",
					timestamp: "2026-07-18T10:03:00.000Z",
					name: "New title",
				}),
			].join("\n"),
		);

		const items = await extractMemoryItems(makeSession(sessionPath));
		expect(items).toHaveLength(2);
		expect(items[0]?.titleText).toBe(normalizeSearchTerms("New title"));
		expect(items.slice(1).every((item) => item.titleText === "")).toBe(true);
	});

	it("keeps workspaces isolated and overlays only unpersisted live conversations", async () => {
		const directory = await createTemporaryDirectory();
		const agentDir = join(directory, "agent");
		const workspaceA = join(directory, "workspace-a");
		const workspaceB = join(directory, "workspace-b");
		expect(getMemoryDatabasePath(agentDir, workspaceA)).not.toBe(getMemoryDatabasePath(agentDir, workspaceB));
		const transient = {
			...makeSession(join(directory, "not-yet-written.jsonl")),
			firstMessage: "后台 runner 仍在执行索引",
			lastMessage: "等待后台 runner 完成",
		};
		const runtime = new MemoryRuntime({ agentDir, cwd: workspaceA, embeddingEngine: new FakeEmbeddingEngine() });
		await runtime.start([]);

		expect((await runtime.search("后台 runner", [transient])).map((result) => result.sessionPath)).toEqual([
			transient.path,
		]);
		await runtime.dispose();
	});

	it("warms the embedding model as memory starts instead of waiting for the first search", async () => {
		const directory = await createTemporaryDirectory();
		const engine = new FakeEmbeddingEngine();
		const runtime = new MemoryRuntime({
			agentDir: join(directory, "agent"),
			cwd: directory,
			embeddingEngine: engine,
		});

		await runtime.start([]);

		expect(engine.prepareCalls).toBe(1);
		expect(runtime.getStatus()).toEqual({ phase: "ready" });
		await runtime.dispose();
	});

	it("presents an empty verified queue as up to date instead of idle", async () => {
		const directory = await createTemporaryDirectory();
		const runtime = new MemoryRuntime({
			agentDir: join(directory, "agent"),
			cwd: directory,
			embeddingEngine: new FakeEmbeddingEngine(),
		});
		await runtime.start([]);

		let diagnostics = await runtime.getDiagnostics();
		for (let attempt = 0; attempt < 50 && diagnostics.indexing.state !== "up-to-date"; attempt++) {
			await new Promise((resolve) => setTimeout(resolve, 10));
			diagnostics = await runtime.getDiagnostics();
		}

		expect(diagnostics.indexing).toEqual({ state: "up-to-date", pending: 0, active: 0 });
		await runtime.dispose();
	});

	it("never marks a zero vector ready", async () => {
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
					message: { role: "user", content: "Index this exchange" },
				}),
				JSON.stringify({
					type: "message",
					id: "a",
					parentId: "u",
					timestamp: "2026-07-18T10:02:00.000Z",
					message: { role: "assistant", content: "Final answer", stopReason: "stop" },
				}),
			].join("\n"),
		);
		const runtime = new MemoryRuntime({
			agentDir: join(directory, "agent"),
			cwd: directory,
			embeddingEngine: new ZeroEmbeddingEngine(),
		});
		await runtime.start([makeSession(sessionPath)]);

		let diagnostics = await runtime.getDiagnostics();
		for (let attempt = 0; attempt < 50 && diagnostics.embeddings.failed === 0; attempt++) {
			await new Promise((resolve) => setTimeout(resolve, 10));
			diagnostics = await runtime.getDiagnostics();
		}
		expect(diagnostics.embeddings).toEqual({ pending: 0, ready: 0, failed: 1 });
		await runtime.dispose();
	});

	it("does not download the semantic model when a normal Bone runtime starts", async () => {
		const directory = await createTemporaryDirectory();
		const agentDir = join(directory, "agent");
		const runtime = new MemoryRuntime({ agentDir, cwd: directory });
		await runtime.start([]);
		expect(await getLocalEmbeddingAvailability(agentDir)).toEqual({ state: "missing" });
		expect(runtime.getStatus()).toEqual({
			phase: "unavailable",
			message: "Keyword search · semantic model not installed. Run bone setup.",
		});
		await runtime.dispose();
	});

	it("requires the verified Q8 GGUF asset before declaring semantic search ready", async () => {
		const directory = await createTemporaryDirectory();
		const agentDir = join(directory, "agent");
		const cacheDirectory = join(agentDir, "models", "bone-semantic-search-v2");
		const revision = "e5708111f19bcfd279811f8f0702d6c33242b402";
		const modelPath = join(
			cacheDirectory,
			"cstr",
			"multilingual-e5-small-GGUF",
			revision,
			"multilingual-e5-small-q8_0.gguf",
		);
		await mkdir(dirname(modelPath), { recursive: true });
		await writeFile(modelPath, "Q8 GGUF model");
		const modelHash = createHash("sha256").update("Q8 GGUF model").digest("hex");
		await writeFile(
			join(cacheDirectory, "asset-manifest.json"),
			JSON.stringify({
				format: "bone-semantic-search-assets-v2",
				modelId: "cstr/multilingual-e5-small-GGUF",
				revision,
				files: { [relative(cacheDirectory, modelPath)]: modelHash },
			}),
		);

		expect(await getLocalEmbeddingAvailability(agentDir)).toEqual({ state: "ready" });

		await writeFile(
			join(cacheDirectory, "asset-manifest.json"),
			JSON.stringify({
				format: "bone-semantic-search-assets-v2",
				modelId: "cstr/multilingual-e5-small-GGUF",
				revision,
				files: {},
			}),
		);
		expect(await getLocalEmbeddingAvailability(agentDir)).toEqual({
			state: "invalid",
			reason: "asset manifest is invalid",
		});
	});

	it("rejects unsafe paths in a local semantic asset manifest", async () => {
		const directory = await createTemporaryDirectory();
		const agentDir = join(directory, "agent");
		const cacheDirectory = join(agentDir, "models", "bone-semantic-search-v2");
		await mkdir(cacheDirectory, { recursive: true });
		await writeFile(
			join(cacheDirectory, "asset-manifest.json"),
			JSON.stringify({
				format: "bone-semantic-search-assets-v2",
				modelId: "cstr/multilingual-e5-small-GGUF",
				revision: "e5708111f19bcfd279811f8f0702d6c33242b402",
				files: {
					"cstr/../../../outside/multilingual-e5-small-q8_0.gguf": "0".repeat(64),
				},
			}),
		);

		expect(await getLocalEmbeddingAvailability(agentDir)).toEqual({
			state: "invalid",
			reason: "asset manifest is invalid",
		});
	});

	it("reports a read-only memory status snapshot without preparing the local model", async () => {
		const directory = await createTemporaryDirectory();
		const agentDir = join(directory, "agent");
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
					message: { role: "user", content: "Add a status command" },
				}),
				JSON.stringify({
					type: "message",
					id: "a",
					parentId: "u",
					timestamp: "2026-07-18T10:02:00.000Z",
					message: { role: "assistant", content: "Implemented /status.", stopReason: "stop" },
				}),
			].join("\n"),
		);
		const runtime = new MemoryRuntime({ agentDir, cwd: directory });
		await runtime.start([makeSession(sessionPath)]);

		expect(await runtime.getDiagnostics()).toMatchObject({
			store: "ready",
			conversations: 1,
			exchanges: 1,
			embeddings: { pending: 1, ready: 0, failed: 0 },
			worker: "not-started",
			vectorIndex: "flat",
			semantic: {
				phase: "unavailable",
				message: "Keyword search · semantic model not installed. Run bone setup.",
			},
		});
		expect(await getLocalEmbeddingAvailability(agentDir)).toEqual({ state: "missing" });
		await runtime.dispose();
	});
});
