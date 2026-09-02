import { constants as bufferConstants } from "buffer";
import { appendFileSync, closeSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync, writeSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
	CURRENT_SESSION_VERSION,
	findMostRecentSession,
	loadEntriesFromFile,
	SessionManager,
} from "../../src/core/session-manager.ts";

function sessionHeader(id: string, cwd = "/tmp"): string {
	return `${JSON.stringify({
		type: "session",
		version: CURRENT_SESSION_VERSION,
		id,
		timestamp: "2025-01-01T00:00:00Z",
		cwd,
	})}\n`;
}

describe("loadEntriesFromFile", () => {
	let tempDir: string;

	beforeEach(() => {
		tempDir = join(tmpdir(), `session-test-${Date.now()}`);
		mkdirSync(tempDir, { recursive: true });
	});

	afterEach(() => {
		rmSync(tempDir, { recursive: true, force: true });
	});

	it("returns empty array for non-existent file", () => {
		const entries = loadEntriesFromFile(join(tempDir, "nonexistent.jsonl"));
		expect(entries).toEqual([]);
	});

	it("returns empty array for empty file", () => {
		const file = join(tempDir, "empty.jsonl");
		writeFileSync(file, "");
		expect(loadEntriesFromFile(file)).toEqual([]);
	});

	it("returns empty array for file without valid session header", () => {
		const file = join(tempDir, "no-header.jsonl");
		writeFileSync(file, '{"type":"message","id":"1"}\n');
		expect(loadEntriesFromFile(file)).toEqual([]);
	});

	it("returns empty array for malformed JSON", () => {
		const file = join(tempDir, "malformed.jsonl");
		writeFileSync(file, "not json\n");
		expect(loadEntriesFromFile(file)).toEqual([]);
	});

	it("loads valid session file", () => {
		const file = join(tempDir, "valid.jsonl");
		writeFileSync(
			file,
			`${JSON.stringify({
				type: "session",
				version: CURRENT_SESSION_VERSION,
				id: "abc",
				timestamp: "2025-01-01T00:00:00Z",
				cwd: "/tmp",
			})}\n` +
				'{"type":"message","id":"1","parentId":null,"timestamp":"2025-01-01T00:00:01Z","message":{"role":"user","content":"hi","timestamp":1}}\n',
		);
		const entries = loadEntriesFromFile(file);
		expect(entries).toHaveLength(2);
		expect(entries[0].type).toBe("session");
		expect(entries[1].type).toBe("message");
	});

	it("rejects a session containing a malformed line", () => {
		const file = join(tempDir, "mixed.jsonl");
		writeFileSync(
			file,
			`${JSON.stringify({
				type: "session",
				version: CURRENT_SESSION_VERSION,
				id: "abc",
				timestamp: "2025-01-01T00:00:00Z",
				cwd: "/tmp",
			})}\n` +
				"not valid json\n" +
				'{"type":"message","id":"1","parentId":null,"timestamp":"2025-01-01T00:00:01Z","message":{"role":"user","content":"hi","timestamp":1}}\n',
		);
		expect(loadEntriesFromFile(file)).toEqual([]);
	});

	it("rejects old session versions and invalid exchange metadata", () => {
		const old = join(tempDir, "old.jsonl");
		writeFileSync(old, '{"type":"session","version":3,"id":"abc","timestamp":"2025-01-01T00:00:00Z","cwd":"/tmp"}\n');
		expect(loadEntriesFromFile(old)).toEqual([]);

		const invalidMetadata = join(tempDir, "invalid-metadata.jsonl");
		writeFileSync(
			invalidMetadata,
			`${JSON.stringify({
				type: "session",
				version: CURRENT_SESSION_VERSION,
				id: "abc",
				timestamp: "2025-01-01T00:00:00Z",
				cwd: "/tmp",
			})}\n` +
				'{"type":"message","id":"1","parentId":null,"timestamp":"2025-01-01T00:00:01Z","exchangeId":"exchange","modelTurnId":"turn","responseDisposition":"done","message":{"role":"assistant","content":[],"provider":"test","model":"test","stopReason":"stop","timestamp":1}}\n',
		);
		expect(loadEntriesFromFile(invalidMetadata)).toEqual([]);
	});

	it("rejects structurally invalid current-version entries", () => {
		const missingBaseFields = join(tempDir, "missing-base-fields.jsonl");
		writeFileSync(
			missingBaseFields,
			`${sessionHeader("abc")}{"type":"session_info","timestamp":"2025-01-01T00:00:01Z","name":"missing id"}\n`,
		);
		expect(loadEntriesFromFile(missingBaseFields)).toEqual([]);

		const invalidMessage = join(tempDir, "invalid-message.jsonl");
		writeFileSync(
			invalidMessage,
			`${sessionHeader("abc")}{"type":"message","id":"1","parentId":null,"timestamp":"2025-01-01T00:00:01Z","message":{"role":"assistant"}}\n`,
		);
		expect(loadEntriesFromFile(invalidMessage)).toEqual([]);

		const invalidToolResult = join(tempDir, "invalid-tool-result.jsonl");
		writeFileSync(
			invalidToolResult,
			`${sessionHeader("abc")}{"type":"message","id":"1","parentId":null,"timestamp":"2025-01-01T00:00:01Z","message":{"role":"toolResult","content":[],"timestamp":1}}\n`,
		);
		expect(loadEntriesFromFile(invalidToolResult)).toEqual([]);

		const invalidContentBlock = join(tempDir, "invalid-content-block.jsonl");
		writeFileSync(
			invalidContentBlock,
			`${sessionHeader("abc")}${JSON.stringify({
				type: "message",
				id: "1",
				parentId: null,
				timestamp: "2025-01-01T00:00:01Z",
				message: {
					role: "assistant",
					content: [null],
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
					timestamp: 1,
				},
			})}\n`,
		);
		expect(loadEntriesFromFile(invalidContentBlock)).toEqual([]);
	});

	it("rejects oversized sparse files containing invalid JSON segments", () => {
		const file = join(tempDir, "large.jsonl");
		writeFileSync(
			file,
			`${JSON.stringify({
				type: "session",
				version: CURRENT_SESSION_VERSION,
				id: "abc",
				timestamp: "2025-01-01T00:00:00Z",
				cwd: "/tmp",
			})}\n`,
		);

		const fd = openSync(file, "r+");
		try {
			const newline = Buffer.from("\n");
			const stride = 16 * 1024 * 1024;
			for (let offset = stride; offset <= bufferConstants.MAX_STRING_LENGTH + stride; offset += stride) {
				writeSync(fd, newline, 0, newline.length, offset);
			}
		} finally {
			closeSync(fd);
		}

		appendFileSync(
			file,
			'{"type":"message","id":"1","parentId":null,"timestamp":"2025-01-01T00:00:01Z","message":{"role":"user","content":"hi","timestamp":1}}\n',
		);

		expect(() => SessionManager.open(file, tempDir)).toThrow("is not a valid pi session");
	});
});

describe("findMostRecentSession", () => {
	let tempDir: string;

	beforeEach(() => {
		tempDir = join(tmpdir(), `session-test-${Date.now()}`);
		mkdirSync(tempDir, { recursive: true });
	});

	afterEach(() => {
		rmSync(tempDir, { recursive: true, force: true });
	});

	it("returns null for empty directory", () => {
		expect(findMostRecentSession(tempDir)).toBeNull();
	});

	it("returns null for non-existent directory", () => {
		expect(findMostRecentSession(join(tempDir, "nonexistent"))).toBeNull();
	});

	it("ignores non-jsonl files", () => {
		writeFileSync(join(tempDir, "file.txt"), "hello");
		writeFileSync(join(tempDir, "file.json"), "{}");
		expect(findMostRecentSession(tempDir)).toBeNull();
	});

	it("ignores jsonl files without valid session header", () => {
		writeFileSync(join(tempDir, "invalid.jsonl"), '{"type":"message"}\n');
		expect(findMostRecentSession(tempDir)).toBeNull();
	});

	it("returns single valid session file", () => {
		const file = join(tempDir, "session.jsonl");
		writeFileSync(file, sessionHeader("abc"));
		expect(findMostRecentSession(tempDir)).toBe(file);
	});

	it("returns most recently modified session", async () => {
		const file1 = join(tempDir, "older.jsonl");
		const file2 = join(tempDir, "newer.jsonl");

		writeFileSync(file1, sessionHeader("old"));
		// Small delay to ensure different mtime
		await new Promise((r) => setTimeout(r, 10));
		writeFileSync(file2, sessionHeader("new"));

		expect(findMostRecentSession(tempDir)).toBe(file2);
	});

	it("skips invalid files and returns valid one", async () => {
		const invalid = join(tempDir, "invalid.jsonl");
		const valid = join(tempDir, "valid.jsonl");

		writeFileSync(invalid, '{"type":"not-session"}\n');
		await new Promise((r) => setTimeout(r, 10));
		writeFileSync(valid, sessionHeader("abc"));

		expect(findMostRecentSession(tempDir)).toBe(valid);
	});

	it("filters most recent session by cwd", async () => {
		const projectA = join(tempDir, "project-a");
		const projectB = join(tempDir, "project-b");
		const fileA = join(tempDir, "a.jsonl");
		const fileB = join(tempDir, "b.jsonl");

		writeFileSync(fileA, sessionHeader("a", projectA));
		await new Promise((r) => setTimeout(r, 10));
		writeFileSync(fileB, sessionHeader("b", projectB));

		expect(findMostRecentSession(tempDir, projectA)).toBe(fileA);
		expect(findMostRecentSession(tempDir, projectB)).toBe(fileB);
	});
});

describe("SessionManager custom flat session directory", () => {
	let tempDir: string;
	let projectA: string;
	let projectB: string;

	beforeEach(() => {
		tempDir = join(tmpdir(), `session-test-${Date.now()}`);
		projectA = join(tempDir, "project-a");
		projectB = join(tempDir, "project-b");
		mkdirSync(projectA, { recursive: true });
		mkdirSync(projectB, { recursive: true });
	});

	afterEach(() => {
		rmSync(tempDir, { recursive: true, force: true });
	});

	function createPersistedSession(cwd: string, label: string): string {
		const session = SessionManager.create(cwd, tempDir);
		session.appendMessage({ role: "user", content: label, timestamp: Date.now() });
		session.appendMessage({
			role: "assistant",
			content: [{ type: "text", text: `reply to ${label}` }],
			api: "anthropic-messages",
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
			stopReason: "stop",
			timestamp: Date.now(),
		});
		const sessionFile = session.getSessionFile();
		if (!sessionFile) {
			throw new Error("Expected persisted session file");
		}
		return sessionFile;
	}

	it("scopes current-folder APIs by cwd while listing all flat sessions", async () => {
		const sessionA = createPersistedSession(projectA, "from A");
		await new Promise((r) => setTimeout(r, 10));
		const sessionB = createPersistedSession(projectB, "from B");

		const currentA = await SessionManager.list(projectA, tempDir);
		expect(currentA.map((session) => session.path)).toEqual([sessionA]);

		const all = await SessionManager.listAll(tempDir);
		expect(new Set(all.map((session) => session.path))).toEqual(new Set([sessionA, sessionB]));

		const continuedA = SessionManager.continueRecent(projectA, tempDir);
		expect(continuedA.getSessionFile()).toBe(sessionA);
	});
});

describe("SessionManager.setSessionFile with corrupted files", () => {
	let tempDir: string;

	beforeEach(() => {
		tempDir = join(tmpdir(), `session-test-${Date.now()}`);
		mkdirSync(tempDir, { recursive: true });
	});

	afterEach(() => {
		rmSync(tempDir, { recursive: true, force: true });
	});

	it("truncates and rewrites empty file with valid header", () => {
		const emptyFile = join(tempDir, "empty.jsonl");
		writeFileSync(emptyFile, "");

		const sm = SessionManager.open(emptyFile, tempDir);

		// Should have created a new session with valid header
		expect(sm.getSessionId()).toBeTruthy();
		expect(sm.getHeader()).toBeTruthy();
		expect(sm.getHeader()?.type).toBe("session");

		// File should now contain a valid header
		const content = readFileSync(emptyFile, "utf-8");
		const lines = content.trim().split("\n").filter(Boolean);
		expect(lines.length).toBe(1);
		const header = JSON.parse(lines[0]);
		expect(header.type).toBe("session");
		expect(header.id).toBe(sm.getSessionId());
	});

	it("throws and preserves non-empty file without valid header", () => {
		const noHeaderFile = join(tempDir, "no-header.jsonl");
		const originalContent =
			'{"type":"message","id":"abc","parentId":"orphaned","timestamp":"2025-01-01T00:00:00Z","message":{"role":"assistant","content":"test"}}\n';
		writeFileSync(noHeaderFile, originalContent);

		expect(() => SessionManager.open(noHeaderFile, tempDir)).toThrow(
			`Session file is not a valid pi session: ${noHeaderFile}`,
		);
		expect(readFileSync(noHeaderFile, "utf-8")).toBe(originalContent);
	});

	it("throws and preserves non-session JSONL files", () => {
		const nonSessionFile = join(tempDir, "not-a-session.log");
		const originalContent = '{"type":"event","data":"not a session"}\n';
		writeFileSync(nonSessionFile, originalContent);

		expect(() => SessionManager.open(nonSessionFile, tempDir)).toThrow(
			`Session file is not a valid pi session: ${nonSessionFile}`,
		);
		expect(readFileSync(nonSessionFile, "utf-8")).toBe(originalContent);
	});

	it("preserves explicit session file path when recovering from corrupted file", () => {
		const explicitPath = join(tempDir, "my-session.jsonl");
		writeFileSync(explicitPath, "");

		const sm = SessionManager.open(explicitPath, tempDir);

		// The session file path should be preserved
		expect(sm.getSessionFile()).toBe(explicitPath);
	});

	it("subsequent loads of initialized empty file work correctly", () => {
		const emptyFile = join(tempDir, "empty.jsonl");
		writeFileSync(emptyFile, "");

		const sm1 = SessionManager.open(emptyFile, tempDir);
		const sessionId = sm1.getSessionId();

		const sm2 = SessionManager.open(emptyFile, tempDir);
		expect(sm2.getSessionId()).toBe(sessionId);
		expect(sm2.getHeader()?.type).toBe("session");
	});
});
