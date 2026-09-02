import { chmodSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { softDeleteSessionFile } from "../src/core/session-trash.ts";

describe("session soft delete", () => {
	const temporaryDirectories: string[] = [];

	afterEach(() => {
		for (const directory of temporaryDirectories.splice(0)) {
			if (existsSync(directory)) rmSync(directory, { recursive: true, force: true });
		}
	});

	function createTemporaryDirectory(): string {
		const directory = join(tmpdir(), `bone-session-trash-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		mkdirSync(directory, { recursive: true });
		temporaryDirectories.push(directory);
		return directory;
	}

	it("uses the system trash command when it succeeds", async () => {
		const directory = createTemporaryDirectory();
		const source = join(directory, "conversation.jsonl");
		const systemTrashDirectory = join(directory, "system-trash");
		const script = join(directory, "fake-trash");
		mkdirSync(systemTrashDirectory);
		writeFileSync(source, "conversation");
		writeFileSync(script, `#!/bin/sh\nmv "$1" "${join(systemTrashDirectory, "conversation.jsonl")}"\n`);
		chmodSync(script, 0o755);

		const result = await softDeleteSessionFile(source, join(directory, "agent"), { trashCommand: script });

		expect(result).toEqual({ ok: true, method: "system-trash" });
		expect(existsSync(source)).toBe(false);
		expect(existsSync(join(systemTrashDirectory, "conversation.jsonl"))).toBe(true);
	});

	it("falls back to Bone Trash with recovery metadata without permanently deleting data", async () => {
		const directory = createTemporaryDirectory();
		const source = join(directory, "conversation.jsonl");
		const agentDir = join(directory, "agent");
		writeFileSync(source, "conversation");

		const result = await softDeleteSessionFile(source, agentDir, { trashCommand: join(directory, "missing-trash") });

		expect(result).toMatchObject({ ok: true, method: "bone-trash" });
		expect(existsSync(source)).toBe(false);
		const trashDirectory = join(agentDir, "trash", "sessions");
		const entries = readdirSync(trashDirectory);
		const metadataPath = join(trashDirectory, entries.find((entry) => entry.endsWith(".json"))!);
		const metadata = JSON.parse(readFileSync(metadataPath, "utf8")) as {
			originalPath: string;
			archivedFileName: string;
		};
		expect(metadata.originalPath).toBe(source);
		expect(existsSync(join(trashDirectory, metadata.archivedFileName))).toBe(true);
	});

	it("refuses paths that are not conversation JSONL files", async () => {
		const directory = createTemporaryDirectory();
		const source = join(directory, "notes.txt");
		writeFileSync(source, "notes");

		await expect(softDeleteSessionFile(source, join(directory, "agent"))).resolves.toEqual({
			ok: false,
			error: "Only .jsonl conversation files can be deleted",
		});
		expect(existsSync(source)).toBe(true);
	});
});
