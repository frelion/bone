import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";
import { encodeTextSignature } from "@frelion/bone-ai";
import { CURRENT_SESSION_VERSION, type SessionInfo, SessionManager } from "@frelion/bone-session";
import { afterEach, describe, expect, it } from "vitest";
import {
	getLocalEmbeddingAvailability,
	getLocalEmbeddingNativeLibraryPath,
	type LocalEmbeddingEngine,
} from "../src/local-embedding.ts";
import { extractMemoryItems, getMemoryDatabasePath, MemoryRuntime } from "../src/memory.ts";
import { normalizeSearchTerms } from "../src/session-search-normalizer.ts";

const temporaryDirectories: string[] = [];

function assistantMsg(text: string) {
	return {
		role: "assistant" as const,
		content: [{ type: "text" as const, text }],
		api: "anthropic-messages" as const,
		provider: "anthropic",
		model: "test",
		usage: {
			input: 1,
			output: 1,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 2,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "stop" as const,
		timestamp: Date.now(),
	};
}

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

function currentSessionJsonl(lines: string[]): string {
	return lines
		.map((line) => {
			const entry = JSON.parse(line) as Record<string, unknown>;
			if (entry.type === "session") {
				return JSON.stringify({ ...entry, version: CURRENT_SESSION_VERSION });
			}
			if (entry.type !== "message") return JSON.stringify(entry);
			const message = entry.message as Record<string, unknown> | undefined;
			const persistedMessage: Record<string, unknown> | undefined = message
				? {
						...message,
						timestamp:
							typeof message.timestamp === "number"
								? message.timestamp
								: Date.parse(String(entry.timestamp ?? new Date(0).toISOString())),
					}
				: undefined;
			if (message?.role === "user") {
				return JSON.stringify({
					...entry,
					exchangeId: entry.exchangeId ?? "exchange-a",
					modelTurnId: entry.modelTurnId ?? "turn-a",
					delivery: entry.delivery ?? "prompt",
					message: persistedMessage,
				});
			}
			if (message?.role === "assistant") {
				const content = Array.isArray(persistedMessage?.content)
					? persistedMessage.content
					: [{ type: "text", text: String(persistedMessage?.content ?? "") }];
				return JSON.stringify({
					...entry,
					exchangeId: entry.exchangeId ?? "exchange-a",
					modelTurnId: entry.modelTurnId ?? "turn-a",
					responseDisposition: entry.responseDisposition ?? "final",
					message: {
						api: "test",
						provider: "test",
						model: "test",
						usage: {
							input: 0,
							output: 0,
							cacheRead: 0,
							cacheWrite: 0,
							totalTokens: 0,
							cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
						},
						stopReason: "stop",
						...persistedMessage,
						content,
					},
				});
			}
			return JSON.stringify({ ...entry, message: persistedMessage });
		})
		.join("\n");
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
	it("resolves the native library from the embedded runtime cache", async () => {
		const runtimeDirectory = await createTemporaryDirectory();
		const platform = `${process.platform}-${process.arch}`;
		const libraryName =
			process.platform === "darwin"
				? "libcrispembed.0.dylib"
				: process.platform === "win32"
					? "crispembed.dll"
					: "libcrispembed.so.0";
		const libraryPath = join(runtimeDirectory, "native", platform, libraryName);
		await mkdir(dirname(libraryPath), { recursive: true });
		await writeFile(libraryPath, "test");
		const previousRuntimeDirectory = process.env.BONE_RUNTIME_DIR;
		process.env.BONE_RUNTIME_DIR = runtimeDirectory;
		try {
			expect(getLocalEmbeddingNativeLibraryPath()).toBe(libraryPath);
		} finally {
			if (previousRuntimeDirectory === undefined) {
				delete process.env.BONE_RUNTIME_DIR;
			} else {
				process.env.BONE_RUNTIME_DIR = previousRuntimeDirectory;
			}
		}
	});

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
			currentSessionJsonl([
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
			]),
		);

		const items = await extractMemoryItems(makeSession(sessionPath));
		expect(items.filter((item) => item.kind === "conversation-exchange")).toHaveLength(1);
		expect(items.find((item) => item.kind === "conversation-exchange")?.semanticText).toContain(
			"Final result: 修复 runtime rebind",
		);
		expect(items.some((item) => item.kind === "file-reference")).toBe(true);
		expect(items.some((item) => item.kind === "command-reference")).toBe(true);
	});

	it("pairs interleaved messages by exchange metadata", async () => {
		const directory = await createTemporaryDirectory();
		const sessionPath = join(directory, "exchange-metadata.jsonl");
		await writeFile(
			sessionPath,
			currentSessionJsonl([
				JSON.stringify({ type: "session", id: "session-a", timestamp: "2026-07-18T10:00:00.000Z", cwd: directory }),
				JSON.stringify({
					type: "message",
					id: "user-a",
					parentId: null,
					timestamp: "2026-07-18T10:01:00.000Z",
					exchangeId: "exchange-a",
					modelTurnId: "turn-a",
					delivery: "prompt",
					message: { role: "user", content: "first task" },
				}),
				JSON.stringify({
					type: "message",
					id: "user-b",
					parentId: "user-a",
					timestamp: "2026-07-18T10:02:00.000Z",
					exchangeId: "exchange-b",
					modelTurnId: "turn-b",
					delivery: "follow_up",
					message: { role: "user", content: "second task" },
				}),
				JSON.stringify({
					type: "message",
					id: "assistant-a",
					parentId: "user-b",
					timestamp: "2026-07-18T10:03:00.000Z",
					exchangeId: "exchange-a",
					modelTurnId: "turn-a2",
					message: { role: "assistant", content: "first result", stopReason: "stop" },
				}),
				JSON.stringify({
					type: "message",
					id: "assistant-b",
					parentId: "assistant-a",
					timestamp: "2026-07-18T10:04:00.000Z",
					exchangeId: "exchange-b",
					modelTurnId: "turn-b",
					message: { role: "assistant", content: "second result", stopReason: "stop" },
				}),
			]),
		);

		const items = (await extractMemoryItems(makeSession(sessionPath))).filter(
			(item) => item.kind === "conversation-exchange",
		);
		expect(items).toHaveLength(2);
		expect(items.map((item) => item.semanticText)).toEqual(
			expect.arrayContaining([
				"User task: first task\nFinal result: first result",
				"User task: second task\nFinal result: second result",
			]),
		);
	});

	it("pairs a persisted custom follow-up with its exchange result", async () => {
		const directory = await createTemporaryDirectory();
		const sessionPath = join(directory, "custom-exchange.jsonl");
		await writeFile(
			sessionPath,
			currentSessionJsonl([
				JSON.stringify({ type: "session", id: "session-a", timestamp: "2026-07-18T10:00:00.000Z", cwd: directory }),
				JSON.stringify({
					type: "custom_message",
					id: "custom-a",
					parentId: null,
					timestamp: "2026-07-18T10:01:00.000Z",
					exchangeId: "exchange-a",
					modelTurnId: "turn-a",
					delivery: "follow_up",
					customType: "extension-task",
					content: "custom task",
					display: false,
				}),
				JSON.stringify({
					type: "message",
					id: "assistant-a",
					parentId: "custom-a",
					timestamp: "2026-07-18T10:02:00.000Z",
					exchangeId: "exchange-a",
					modelTurnId: "turn-a",
					message: { role: "assistant", content: "custom result", stopReason: "stop" },
				}),
			]),
		);

		const exchange = (await extractMemoryItems(makeSession(sessionPath))).find(
			(item) => item.kind === "conversation-exchange",
		);
		expect(exchange?.semanticText).toBe("User task: custom task\nFinal result: custom result");
	});

	it("indexes only explicit final-answer blocks from mixed assistant output", async () => {
		const directory = await createTemporaryDirectory();
		const sessionPath = join(directory, "mixed-phases.jsonl");
		await writeFile(
			sessionPath,
			currentSessionJsonl([
				JSON.stringify({ type: "session", id: "session-a", timestamp: "2026-07-18T10:00:00.000Z", cwd: directory }),
				JSON.stringify({
					type: "message",
					id: "user-a",
					parentId: null,
					timestamp: "2026-07-18T10:01:00.000Z",
					exchangeId: "exchange-a",
					message: { role: "user", content: "phase-aware task" },
				}),
				JSON.stringify({
					type: "message",
					id: "assistant-a",
					parentId: "user-a",
					timestamp: "2026-07-18T10:02:00.000Z",
					exchangeId: "exchange-a",
					message: {
						role: "assistant",
						content: [
							{
								type: "text",
								text: "internal stage update",
								textSignature: encodeTextSignature("c", "commentary"),
							},
							{
								type: "text",
								text: "user-facing result",
								textSignature: encodeTextSignature("f", "final_answer"),
							},
						],
						stopReason: "stop",
					},
				}),
			]),
		);

		const exchange = (await extractMemoryItems(makeSession(sessionPath))).find(
			(item) => item.kind === "conversation-exchange",
		);
		expect(exchange?.semanticText).toContain("Final result: user-facing result");
		expect(exchange?.semanticText).not.toContain("internal stage update");
	});

	it("ignores rejected final-answer text until the exchange has an accepted final response", async () => {
		const directory = await createTemporaryDirectory();
		const sessionPath = join(directory, "rejected-final.jsonl");
		await writeFile(
			sessionPath,
			currentSessionJsonl([
				JSON.stringify({ type: "session", id: "session-a", timestamp: "2026-07-18T10:00:00.000Z", cwd: directory }),
				JSON.stringify({
					type: "message",
					id: "user-a",
					parentId: null,
					timestamp: "2026-07-18T10:01:00.000Z",
					exchangeId: "exchange-a",
					delivery: "prompt",
					message: { role: "user", content: "ship safely" },
				}),
				JSON.stringify({
					type: "message",
					id: "assistant-rejected",
					parentId: "user-a",
					timestamp: "2026-07-18T10:02:00.000Z",
					exchangeId: "exchange-a",
					responseDisposition: "rejected",
					message: {
						role: "assistant",
						content: [
							{
								type: "text",
								text: "unsafe result",
								textSignature: encodeTextSignature("bad", "final_answer"),
							},
						],
						stopReason: "stop",
					},
				}),
				JSON.stringify({
					type: "message",
					id: "assistant-final",
					parentId: "assistant-rejected",
					timestamp: "2026-07-18T10:03:00.000Z",
					exchangeId: "exchange-a",
					responseDisposition: "final",
					message: { role: "assistant", content: "accepted result", stopReason: "stop" },
				}),
			]),
		);

		const exchange = (await extractMemoryItems(makeSession(sessionPath))).find(
			(item) => item.kind === "conversation-exchange",
		);
		expect(exchange?.semanticText).toContain("Final result: accepted result");
		expect(exchange?.semanticText).not.toContain("unsafe result");
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
			currentSessionJsonl([
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
			]),
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
			currentSessionJsonl([
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
			]),
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
			currentSessionJsonl([
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
			]),
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
			currentSessionJsonl([
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
			]),
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
			currentSessionJsonl([
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
			]),
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
