import { createHash } from "node:crypto";
import { createReadStream, existsSync } from "node:fs";
import { mkdir, open, readdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join, relative } from "node:path";
import lockfile from "proper-lockfile";
import {
	getLocalEmbeddingCacheDirectory,
	getLocalEmbeddingManifestPath,
	isLocalEmbeddingAssetManifest,
	LOCAL_EMBEDDING_GGUF_FILE,
	LOCAL_EMBEDDING_MODEL_ID,
	type LocalEmbeddingAssetManifest,
} from "./local-embedding-assets.ts";

const MODEL_API_URL = `https://huggingface.co/api/models/${LOCAL_EMBEDDING_MODEL_ID}`;
const REQUEST_TIMEOUT_MS = 20_000;

type SetupStatus =
	| { phase: "downloading"; loadedBytes?: number; totalBytes?: number; file?: string }
	| { phase: "loading" }
	| { phase: "ready" };

interface BunWorkerPort {
	postMessage(message: unknown): void;
	onmessage: ((event: { data: unknown }) => void) | null;
}

const port = globalThis as unknown as BunWorkerPort;

function cacheDirectory(agentDir: string): string {
	return getLocalEmbeddingCacheDirectory(agentDir);
}

function manifestPath(agentDir: string): string {
	return getLocalEmbeddingManifestPath(agentDir);
}

async function fileHash(path: string): Promise<string> {
	const hash = createHash("sha256");
	for await (const chunk of createReadStream(path)) hash.update(chunk);
	return hash.digest("hex");
}

async function listCachedFiles(root: string): Promise<string[]> {
	const entries = await readdir(root, { withFileTypes: true });
	const files: string[] = [];
	for (const entry of entries) {
		const path = join(root, entry.name);
		if (entry.isDirectory()) files.push(...(await listCachedFiles(path)));
		else if (entry.isFile()) files.push(path);
	}
	return files;
}

async function readVerifiedManifest(agentDir: string): Promise<LocalEmbeddingAssetManifest | undefined> {
	const path = manifestPath(agentDir);
	if (!existsSync(path)) return undefined;
	try {
		const manifest: unknown = JSON.parse(await readFile(path, "utf8"));
		if (!isLocalEmbeddingAssetManifest(manifest) || Object.keys(manifest.files).length === 0) return undefined;
		const root = cacheDirectory(agentDir);
		for (const [file, expectedHash] of Object.entries(manifest.files)) {
			const filePath = join(root, file);
			if (!existsSync(filePath) || (await fileHash(filePath)) !== expectedHash) return undefined;
		}
		return manifest;
	} catch {
		return undefined;
	}
}

function postStatus(status: SetupStatus): void {
	port.postMessage({ type: "status", status });
}

async function resolveImmutableRevision(): Promise<string> {
	const response = await fetch(MODEL_API_URL, { signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS) });
	if (!response.ok) throw new Error(`Could not resolve local semantic model revision (${response.status})`);
	const payload: unknown = await response.json();
	const sha = payload && typeof payload === "object" ? (payload as { sha?: unknown }).sha : undefined;
	if (typeof sha !== "string" || !/^[0-9a-f]{40}$/i.test(sha)) {
		throw new Error("Local semantic model registry returned an invalid revision");
	}
	return sha;
}

async function downloadModel(agentDir: string, revision: string): Promise<void> {
	const target = join(cacheDirectory(agentDir), LOCAL_EMBEDDING_MODEL_ID, revision, LOCAL_EMBEDDING_GGUF_FILE);
	if (existsSync(target)) return;
	const temporary = `${target}.${process.pid}.${Date.now()}.tmp`;
	const response = await fetch(
		`https://huggingface.co/${LOCAL_EMBEDDING_MODEL_ID}/resolve/${revision}/${LOCAL_EMBEDDING_GGUF_FILE}`,
		{
			signal: AbortSignal.timeout(10 * 60_000),
		},
	);
	if (!response.ok || !response.body) {
		throw new Error(`Could not download ${LOCAL_EMBEDDING_GGUF_FILE} (${response.status})`);
	}
	const totalBytes = Number(response.headers.get("content-length"));
	let loadedBytes = 0;
	await mkdir(dirname(target), { recursive: true, mode: 0o700 });
	const file = await open(temporary, "w", 0o600);
	try {
		for await (const chunk of response.body) {
			await file.write(chunk);
			loadedBytes += chunk.length;
			postStatus({
				phase: "downloading",
				file: LOCAL_EMBEDDING_GGUF_FILE,
				loadedBytes,
				...(Number.isFinite(totalBytes) && totalBytes > 0 ? { totalBytes } : {}),
			});
		}
	} catch (error) {
		await rm(temporary, { force: true });
		throw error;
	} finally {
		await file.close();
	}
	await rename(temporary, target);
}

async function writeManifest(agentDir: string, revision: string): Promise<void> {
	const root = cacheDirectory(agentDir);
	const revisionRoot = join(root, LOCAL_EMBEDDING_MODEL_ID, revision);
	const files = Object.fromEntries(
		await Promise.all(
			(await listCachedFiles(revisionRoot)).map(
				async (path) => [relative(root, path), await fileHash(path)] as const,
			),
		),
	);
	const temporary = `${manifestPath(agentDir)}.${process.pid}.${Date.now()}.tmp`;
	await writeFile(
		temporary,
		`${JSON.stringify(
			{ format: "bone-semantic-search-assets-v2", modelId: LOCAL_EMBEDDING_MODEL_ID, revision, files },
			null,
			2,
		)}\n`,
		{ mode: 0o600 },
	);
	await rename(temporary, manifestPath(agentDir));
}

async function prepare(agentDir: string): Promise<void> {
	const cacheDir = cacheDirectory(agentDir);
	await mkdir(cacheDir, { recursive: true, mode: 0o700 });
	const release = await lockfile.lock(cacheDir, {
		realpath: false,
		stale: 120_000,
		retries: { retries: 8, factor: 1.5, minTimeout: 200, maxTimeout: 2_000 },
	});
	try {
		if (await readVerifiedManifest(agentDir)) return;
		await rm(manifestPath(agentDir), { force: true });
		postStatus({ phase: "downloading", file: LOCAL_EMBEDDING_GGUF_FILE });
		const revision = await resolveImmutableRevision();
		await rm(join(cacheDir, LOCAL_EMBEDDING_MODEL_ID, revision), { recursive: true, force: true });
		await downloadModel(agentDir, revision);
		await writeManifest(agentDir, revision);
		if (!(await readVerifiedManifest(agentDir))) {
			throw new Error("Local semantic model asset verification failed after download");
		}
		postStatus({ phase: "ready" });
	} finally {
		await release();
	}
}

port.onmessage = (event) => {
	const message = event.data;
	const agentDir = message && typeof message === "object" ? (message as { agentDir?: unknown }).agentDir : undefined;
	if (typeof agentDir !== "string") {
		port.postMessage({ type: "error", message: "Invalid local embedding setup request" });
		return;
	}
	void prepare(agentDir)
		.then(() => port.postMessage({ type: "complete" }))
		.catch((error) =>
			port.postMessage({ type: "error", message: error instanceof Error ? error.message : String(error) }),
		);
};
