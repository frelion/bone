import { spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { copyFile, lstat, mkdir, rename, unlink, writeFile } from "node:fs/promises";
import { extname, join } from "node:path";
import { resolvePath } from "../utils/paths.ts";

const TRASH_METADATA_VERSION = 1;

export type SessionTrashMethod = "system-trash" | "bone-trash";

export interface SoftDeletedSessionMetadata {
	version: typeof TRASH_METADATA_VERSION;
	originalPath: string;
	archivedFileName: string;
	deletedAt: string;
}

export type SoftDeleteSessionResult =
	| { ok: true; method: SessionTrashMethod; archivedPath?: string }
	| { ok: false; error: string };

export interface SoftDeleteSessionOptions {
	/** Override for deterministic tests and packaged environments with a custom trash client. */
	trashCommand?: string;
}

function formatSystemTrashError(error: unknown): string | undefined {
	if (!(error instanceof Error)) return undefined;
	return error.message.slice(0, 200);
}

async function moveToBoneTrash(sessionPath: string, agentDir: string): Promise<SoftDeleteSessionResult> {
	const trashDir = join(resolvePath(agentDir), "trash", "sessions");
	const archiveId = `${Date.now()}-${randomUUID()}`;
	const archivedFileName = `${archiveId}.jsonl`;
	const archivedPath = join(trashDir, archivedFileName);
	const metadataPath = join(trashDir, `${archiveId}.json`);
	const metadataTemporaryPath = `${metadataPath}.${randomUUID()}.tmp`;
	const metadata: SoftDeletedSessionMetadata = {
		version: TRASH_METADATA_VERSION,
		originalPath: sessionPath,
		archivedFileName,
		deletedAt: new Date().toISOString(),
	};

	try {
		await mkdir(trashDir, { recursive: true, mode: 0o700 });
		await writeFile(metadataTemporaryPath, `${JSON.stringify(metadata, null, "\t")}\n`, {
			encoding: "utf8",
			mode: 0o600,
		});
		await rename(metadataTemporaryPath, metadataPath);

		try {
			await rename(sessionPath, archivedPath);
		} catch (error) {
			if (!(error instanceof Error) || !("code" in error) || error.code !== "EXDEV") {
				throw error;
			}
			const temporaryArchivePath = `${archivedPath}.${randomUUID()}.tmp`;
			try {
				await copyFile(sessionPath, temporaryArchivePath);
				await rename(temporaryArchivePath, archivedPath);
				await unlink(sessionPath);
			} catch (copyError) {
				await unlink(temporaryArchivePath).catch(() => {});
				throw copyError;
			}
		}

		return { ok: true, method: "bone-trash", archivedPath };
	} catch (error) {
		await unlink(metadataTemporaryPath).catch(() => {});
		await unlink(metadataPath).catch(() => {});
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}
}

/**
 * Soft-delete a persisted conversation. A system trash command is preferred;
 * when it is unavailable, Bone stores the session and recovery metadata in its
 * private trash directory. This function never falls back to permanent delete.
 */
export async function softDeleteSessionFile(
	sessionPath: string,
	agentDir: string,
	options: SoftDeleteSessionOptions = {},
): Promise<SoftDeleteSessionResult> {
	const resolvedSessionPath = resolvePath(sessionPath);
	if (extname(resolvedSessionPath) !== ".jsonl") {
		return { ok: false, error: "Only .jsonl conversation files can be deleted" };
	}

	try {
		const stats = await lstat(resolvedSessionPath);
		if (!stats.isFile()) {
			return { ok: false, error: "Conversation path is not a regular file" };
		}
	} catch (error) {
		return { ok: false, error: error instanceof Error ? error.message : String(error) };
	}

	const trashArgs = resolvedSessionPath.startsWith("-") ? ["--", resolvedSessionPath] : [resolvedSessionPath];
	const systemTrash = spawnSync(options.trashCommand ?? "trash", trashArgs, { encoding: "utf8" });
	if (systemTrash.status === 0) {
		return { ok: true, method: "system-trash" };
	}

	const fallback = await moveToBoneTrash(resolvedSessionPath, agentDir);
	if (fallback.ok) return fallback;

	const systemError = formatSystemTrashError(systemTrash.error);
	const stderr = systemTrash.stderr.trim().split("\n")[0];
	const details = [systemError, stderr].filter((value): value is string => Boolean(value)).join(" · ");
	return {
		ok: false,
		error: details ? `${fallback.error} (system trash: ${details})` : fallback.error,
	};
}
