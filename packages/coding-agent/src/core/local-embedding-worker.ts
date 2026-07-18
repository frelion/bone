import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, readdir, readFile, rename, rm, writeFile } from "node:fs/promises";
import { join, relative } from "node:path";
import { parentPort } from "node:worker_threads";
import { env, pipeline } from "@huggingface/transformers";
import lockfile from "proper-lockfile";

const MODEL_ID = "Xenova/multilingual-e5-small";
const MODEL_CACHE_DIRECTORY = "bone-semantic-search-v1";
const MODEL_API_URL = `https://huggingface.co/api/models/${MODEL_ID}`;
const MODEL_REVISION_REQUEST_TIMEOUT_MS = 20_000;

interface ModelAssetManifest {
	format: "bone-semantic-search-assets-v1";
	modelId: string;
	revision: string;
	files: Record<string, string>;
}

interface EmbedRequest {
	id: number;
	kind: "embed";
	agentDir: string;
	mode: "query" | "document";
	texts: string[];
}

interface DisposeRequest {
	id: number;
	kind: "dispose";
}

type WorkerRequest = EmbedRequest | DisposeRequest;

interface WorkerResponse {
	id: number;
	vectors?: number[][];
	error?: string;
	status?:
		| { phase: "downloading"; loadedBytes?: number; totalBytes?: number; file?: string }
		| { phase: "loading" }
		| { phase: "ready" };
}

function isWorkerRequest(value: unknown): value is WorkerRequest {
	if (!value || typeof value !== "object") return false;
	const candidate = value as Partial<WorkerRequest>;
	return typeof candidate.id === "number" && (candidate.kind === "dispose" || candidate.kind === "embed");
}

type FeatureExtractor = (
	texts: string[],
	options: { pooling: "mean"; normalize: true },
) => Promise<{ tolist(): number[][] }>;

let extractor: FeatureExtractor | undefined;

function cacheDirectory(agentDir: string): string {
	return join(agentDir, "models", MODEL_CACHE_DIRECTORY);
}

function manifestPath(agentDir: string): string {
	return join(cacheDirectory(agentDir), "asset-manifest.json");
}

function isManifest(value: unknown): value is ModelAssetManifest {
	if (!value || typeof value !== "object") return false;
	const candidate = value as Partial<ModelAssetManifest>;
	return (
		candidate.format === "bone-semantic-search-assets-v1" &&
		candidate.modelId === MODEL_ID &&
		typeof candidate.revision === "string" &&
		/^[0-9a-f]{40}$/i.test(candidate.revision) &&
		candidate.files !== undefined &&
		candidate.files !== null &&
		typeof candidate.files === "object" &&
		Object.entries(candidate.files).every(
			([path, hash]) => path.length > 0 && typeof hash === "string" && /^[0-9a-f]{64}$/i.test(hash),
		)
	);
}

async function fileHash(path: string): Promise<string> {
	return createHash("sha256")
		.update(await readFile(path))
		.digest("hex");
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

async function readVerifiedManifest(agentDir: string): Promise<ModelAssetManifest | undefined> {
	const path = manifestPath(agentDir);
	if (!existsSync(path)) return undefined;
	try {
		const manifest: unknown = JSON.parse(await readFile(path, "utf8"));
		if (!isManifest(manifest) || Object.keys(manifest.files).length === 0) return undefined;
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

async function resolveImmutableRevision(): Promise<string> {
	const response = await fetch(MODEL_API_URL, { signal: AbortSignal.timeout(MODEL_REVISION_REQUEST_TIMEOUT_MS) });
	if (!response.ok) throw new Error(`Could not resolve local semantic model revision (${response.status})`);
	const payload: unknown = await response.json();
	const sha = payload && typeof payload === "object" ? (payload as { sha?: unknown }).sha : undefined;
	if (typeof sha !== "string" || !/^[0-9a-f]{40}$/i.test(sha))
		throw new Error("Local semantic model registry returned an invalid revision");
	return sha;
}

function postStatus(status: NonNullable<WorkerResponse["status"]>): void {
	parentPort?.postMessage({ id: 0, status } satisfies WorkerResponse);
}

function formatProgress(progress: unknown): { loadedBytes?: number; totalBytes?: number; file?: string } {
	if (!progress || typeof progress !== "object") return {};
	const value = progress as { loaded?: unknown; total?: unknown; file?: unknown };
	return {
		...(typeof value.loaded === "number" && Number.isFinite(value.loaded) ? { loadedBytes: value.loaded } : {}),
		...(typeof value.total === "number" && Number.isFinite(value.total) ? { totalBytes: value.total } : {}),
		...(typeof value.file === "string" ? { file: value.file } : {}),
	};
}

async function writeManifest(agentDir: string, revision: string): Promise<void> {
	const root = cacheDirectory(agentDir);
	const revisionRoot = join(root, MODEL_ID, revision);
	const resolvedFiles = Object.fromEntries(
		await Promise.all(
			(await listCachedFiles(revisionRoot)).map(
				async (path) => [relative(root, path), await fileHash(path)] as const,
			),
		),
	);
	const manifest: ModelAssetManifest = {
		format: "bone-semantic-search-assets-v1",
		modelId: MODEL_ID,
		revision,
		files: resolvedFiles,
	};
	const target = manifestPath(agentDir);
	const temporary = `${target}.${process.pid}.${Date.now()}.tmp`;
	await writeFile(temporary, `${JSON.stringify(manifest, null, 2)}\n`, { mode: 0o600 });
	await rename(temporary, target);
}

async function loadExtractor(agentDir: string): Promise<NonNullable<typeof extractor>> {
	if (extractor) return extractor;
	const cacheDir = cacheDirectory(agentDir);
	await mkdir(cacheDir, { recursive: true, mode: 0o700 });
	const release = await lockfile.lock(cacheDir, {
		realpath: false,
		stale: 120_000,
		retries: { retries: 8, factor: 1.5, minTimeout: 200, maxTimeout: 2_000 },
	});
	try {
		if (extractor) return extractor;
		let manifest = await readVerifiedManifest(agentDir);
		if (!manifest) {
			await rm(manifestPath(agentDir), { force: true });
			postStatus({ phase: "downloading" });
			const revision = await resolveImmutableRevision();
			await rm(join(cacheDir, MODEL_ID, revision), { recursive: true, force: true });
			env.allowRemoteModels = true;
			env.cacheDir = cacheDir;
			const loaded = await pipeline("feature-extraction", MODEL_ID, {
				dtype: "q8",
				revision,
				progress_callback: (progress: unknown) => postStatus({ phase: "downloading", ...formatProgress(progress) }),
			});
			extractor = loaded as unknown as FeatureExtractor;
			await writeManifest(agentDir, revision);
			manifest = await readVerifiedManifest(agentDir);
			if (!manifest) throw new Error("Local semantic model asset verification failed after download");
		}
		postStatus({ phase: "loading" });
		env.cacheDir = cacheDir;
		env.allowRemoteModels = false;
		if (!extractor) {
			const loaded = await pipeline("feature-extraction", MODEL_ID, {
				dtype: "q8",
				revision: manifest.revision,
				local_files_only: true,
			});
			extractor = loaded as unknown as FeatureExtractor;
		}
	} finally {
		await release();
	}
	postStatus({ phase: "ready" });
	return extractor;
}

async function embed(request: EmbedRequest): Promise<number[][]> {
	const pipeline = await loadExtractor(request.agentDir);
	const prefix = request.mode === "query" ? "query: Find the previous Bone conversation relevant to: " : "passage: ";
	const output = await pipeline(
		request.texts.map((text) => `${prefix}${text}`),
		{ pooling: "mean", normalize: true },
	);
	return output.tolist();
}

parentPort?.on("message", (message: unknown) => {
	if (!isWorkerRequest(message)) return;
	void (async () => {
		try {
			if (message.kind === "dispose") {
				extractor = undefined;
				parentPort?.postMessage({ id: message.id } satisfies WorkerResponse);
				return;
			}
			parentPort?.postMessage({ id: message.id, vectors: await embed(message) } satisfies WorkerResponse);
		} catch (error) {
			parentPort?.postMessage({
				id: message.id,
				error: error instanceof Error ? error.message : String(error),
			} satisfies WorkerResponse);
		}
	})();
});
