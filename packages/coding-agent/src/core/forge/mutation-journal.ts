import { randomUUID } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import lockfile from "proper-lockfile";

export type ForgeMutationStatus = "pending" | "completed" | "failed" | "ambiguous";

export interface ForgeMutationEntry {
	requestId: string;
	fingerprint: string;
	status: ForgeMutationStatus;
	createdAt: string;
	updatedAt: string;
	result?: unknown;
	error?: string;
}

export type ForgeMutationBegin =
	| { action: "execute"; entry: ForgeMutationEntry }
	| { action: "replay"; entry: ForgeMutationEntry; result: unknown }
	| { action: "in_progress" | "ambiguous"; entry: ForgeMutationEntry };

type JournalData = Record<string, ForgeMutationEntry>;

function acquire(path: string): () => void {
	mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
	for (let attempt = 1; attempt <= 10; attempt++) {
		try {
			return lockfile.lockSync(dirname(path), { realpath: false, lockfilePath: `${path}.lock` });
		} catch (error) {
			const code =
				typeof error === "object" && error !== null && "code" in error
					? String((error as { code?: unknown }).code)
					: undefined;
			if (code !== "ELOCKED" || attempt === 10) throw error;
			const started = Date.now();
			while (Date.now() - started < 20) {
				// Keep the journal API synchronous while waiting briefly for another writer.
			}
		}
	}
	throw new Error("Failed to acquire Forge mutation journal lock");
}

function read(path: string): JournalData {
	if (!existsSync(path)) return {};
	let parsed: unknown;
	try {
		parsed = JSON.parse(readFileSync(path, "utf8"));
	} catch (error) {
		throw new Error(`Failed to read Forge mutation journal: ${error instanceof Error ? error.message : error}`);
	}
	if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
		throw new Error("Invalid Forge mutation journal");
	}
	return parsed as JournalData;
}

function write(path: string, data: JournalData): void {
	const temporary = `${path}.${randomUUID()}.tmp`;
	try {
		writeFileSync(temporary, `${JSON.stringify(data, null, 2)}\n`, { encoding: "utf8", mode: 0o600 });
		chmodSync(temporary, 0o600);
		renameSync(temporary, path);
		chmodSync(path, 0o600);
	} catch (error) {
		if (existsSync(temporary)) unlinkSync(temporary);
		throw error;
	}
}

export class ForgeMutationJournal {
	private readonly path: string;

	constructor(agentDir: string) {
		this.path = join(agentDir, "forge-mutations.json");
	}

	begin(requestId: string, fingerprint: string): ForgeMutationBegin {
		if (!requestId || !fingerprint) throw new Error("Forge requestId and fingerprint are required");
		const release = acquire(this.path);
		try {
			const data = read(this.path);
			const existing = data[requestId];
			if (existing) {
				if (existing.fingerprint !== fingerprint) {
					throw new ForgeMutationConflictError(requestId);
				}
				if (existing.status === "completed") return { action: "replay", entry: existing, result: existing.result };
				if (existing.status === "ambiguous") return { action: "ambiguous", entry: existing };
				if (existing.status === "pending") return { action: "in_progress", entry: existing };
			}
			const now = new Date().toISOString();
			const entry: ForgeMutationEntry = {
				requestId,
				fingerprint,
				status: "pending",
				createdAt: now,
				updatedAt: now,
			};
			data[requestId] = entry;
			write(this.path, data);
			return { action: "execute", entry };
		} finally {
			release();
		}
	}

	complete(requestId: string, fingerprint: string, result: unknown): ForgeMutationEntry {
		return this.transition(requestId, fingerprint, "completed", { result });
	}

	fail(requestId: string, fingerprint: string, error: string): ForgeMutationEntry {
		return this.transition(requestId, fingerprint, "failed", { error });
	}

	markAmbiguous(requestId: string, fingerprint: string, error?: string): ForgeMutationEntry {
		return this.transition(requestId, fingerprint, "ambiguous", { error });
	}

	private transition(
		requestId: string,
		fingerprint: string,
		status: Exclude<ForgeMutationStatus, "pending">,
		fields: { result?: unknown; error?: string },
	): ForgeMutationEntry {
		const release = acquire(this.path);
		try {
			const data = read(this.path);
			const existing = data[requestId];
			if (!existing || existing.fingerprint !== fingerprint) throw new ForgeMutationConflictError(requestId);
			if (existing.status !== "pending" && !(existing.status === "failed" && status === "ambiguous")) {
				throw new Error(`Cannot transition Forge mutation ${requestId} from ${existing.status} to ${status}`);
			}
			const entry: ForgeMutationEntry = { ...existing, ...fields, status, updatedAt: new Date().toISOString() };
			data[requestId] = entry;
			write(this.path, data);
			return entry;
		} finally {
			release();
		}
	}
}

export class ForgeMutationConflictError extends Error {
	readonly code = "conflict";

	constructor(requestId: string) {
		super(`Forge requestId ${requestId} was already used for a different operation`);
		this.name = "ForgeMutationConflictError";
	}
}
