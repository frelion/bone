import { randomUUID } from "node:crypto";
import {
	chmodSync,
	closeSync,
	existsSync,
	fsyncSync,
	mkdirSync,
	openSync,
	readFileSync,
	renameSync,
	rmSync,
	statSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";

const JOURNAL_FILE_NAME = ".bone-settings-transaction.json";
const JOURNAL_VERSION = 1;

type JournalEntry = {
	path: string;
	existed: boolean;
	mode?: number;
	contents?: string;
};

type JournalDocument = {
	version: typeof JOURNAL_VERSION;
	id: string;
	phase: "prepared" | "applying";
	entries: JournalEntry[];
};

function fsyncFile(path: string): void {
	const descriptor = openSync(path, "r");
	try {
		fsyncSync(descriptor);
	} finally {
		closeSync(descriptor);
	}
}

function writeAtomic(path: string, contents: string, mode = 0o600): void {
	mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
	const temporary = `${path}.${randomUUID()}.tmp`;
	try {
		writeFileSync(temporary, contents, { encoding: "utf8", mode });
		chmodSync(temporary, mode);
		fsyncFile(temporary);
		renameSync(temporary, path);
		chmodSync(path, mode);
		fsyncFile(path);
	} catch (error) {
		if (existsSync(temporary)) rmSync(temporary, { force: true });
		throw error;
	}
}

/**
 * Durable before-image journal for the Settings Center's multi-file save.
 *
 * A process crash can interrupt independent atomic renames. The journal is
 * made durable before any mutation; startup sees a surviving journal and
 * restores all recorded files to their exact pre-save bytes. This intentionally
 * chooses a consistent rollback over attempting to infer which renames won.
 */
export class SettingsTransactionJournal {
	private readonly path: string;
	private readonly document: JournalDocument;
	private finished = false;

	private constructor(path: string, document: JournalDocument) {
		this.path = path;
		this.document = document;
	}

	static begin(agentDir: string, paths: readonly string[]): SettingsTransactionJournal {
		const journalPath = join(resolve(agentDir), JOURNAL_FILE_NAME);
		SettingsTransactionJournal.recover(agentDir);
		const entries = [...new Set(paths.map((path) => resolve(path)))].map((path): JournalEntry => {
			if (!existsSync(path)) return { path, existed: false };
			const stat = statSync(path);
			return {
				path,
				existed: true,
				mode: stat.mode & 0o777,
				contents: readFileSync(path).toString("base64"),
			};
		});
		const document: JournalDocument = { version: JOURNAL_VERSION, id: randomUUID(), phase: "prepared", entries };
		writeAtomic(journalPath, `${JSON.stringify(document)}\n`);
		return new SettingsTransactionJournal(journalPath, document);
	}

	/** Call immediately before the first target-file mutation. */
	markApplying(): void {
		if (this.finished) throw new Error("Settings transaction has already finished");
		this.document.phase = "applying";
		writeAtomic(this.path, `${JSON.stringify(this.document)}\n`);
	}

	/** All target files are durable and mutually consistent; remove recovery state. */
	commit(): void {
		if (this.finished) return;
		if (existsSync(this.path)) unlinkSync(this.path);
		this.finished = true;
	}

	/** Restore the durable before-image after a caught transaction failure. */
	rollback(): void {
		if (this.finished) return;
		SettingsTransactionJournal.restore(this.document);
		if (existsSync(this.path)) unlinkSync(this.path);
		this.finished = true;
	}

	/** Recovery is idempotent and intentionally ignores malformed optional journals. */
	static recover(agentDir: string): boolean {
		const path = join(resolve(agentDir), JOURNAL_FILE_NAME);
		if (!existsSync(path)) return false;
		try {
			const parsed: unknown = JSON.parse(readFileSync(path, "utf8"));
			if (!SettingsTransactionJournal.isDocument(parsed)) return false;
			SettingsTransactionJournal.restore(parsed);
			unlinkSync(path);
			return true;
		} catch {
			// Preserve a malformed journal for forensic inspection rather than deleting it.
			return false;
		}
	}

	private static restore(document: JournalDocument): void {
		for (const entry of document.entries) {
			if (!entry.existed) {
				rmSync(entry.path, { force: true });
				continue;
			}
			writeAtomic(entry.path, Buffer.from(entry.contents ?? "", "base64").toString("utf8"), entry.mode ?? 0o600);
		}
	}

	private static isDocument(value: unknown): value is JournalDocument {
		return (
			typeof value === "object" &&
			value !== null &&
			(value as { version?: unknown }).version === JOURNAL_VERSION &&
			((value as { phase?: unknown }).phase === "prepared" || (value as { phase?: unknown }).phase === "applying") &&
			Array.isArray((value as { entries?: unknown }).entries)
		);
	}
}
