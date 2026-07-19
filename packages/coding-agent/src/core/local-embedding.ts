import { createHash } from "node:crypto";
import { createReadStream, existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { Worker } from "node:worker_threads";

const MODEL_ID = "cstr/multilingual-e5-small-GGUF";
const MODEL_CACHE_DIRECTORY = "bone-semantic-search-v2";
const GGUF_MODEL_FILE = "multilingual-e5-small-q8_0.gguf";

export type LocalEmbeddingAvailability =
	| { state: "ready" }
	| { state: "missing" }
	| { state: "invalid"; reason: string };

export type LocalEmbeddingStatus =
	| { phase: "downloading"; loadedBytes?: number; totalBytes?: number; file?: string }
	| { phase: "loading" }
	| { phase: "ready" };

/** Minimal boundary used by search so tests never need to start the native GGUF runtime. */
export interface LocalEmbeddingEngine {
	/** Resolve and verify the fixed local model before any search query needs it. */
	prepare(): Promise<void>;
	embedQuery(query: string): Promise<Float32Array>;
	embedDocuments(documents: readonly string[]): Promise<Float32Array[]>;
	dispose(): Promise<void>;
}

interface ModelAssetManifest {
	format: "bone-semantic-search-assets-v2";
	modelId: string;
	revision: string;
	files: Record<string, string>;
}

function getCacheDirectory(agentDir: string): string {
	return join(agentDir, "models", MODEL_CACHE_DIRECTORY);
}

function getManifestPath(agentDir: string): string {
	return join(getCacheDirectory(agentDir), "asset-manifest.json");
}

function isAssetManifest(value: unknown): value is ModelAssetManifest {
	if (!value || typeof value !== "object") return false;
	const candidate = value as Partial<ModelAssetManifest>;
	return (
		candidate.format === "bone-semantic-search-assets-v2" &&
		candidate.modelId === MODEL_ID &&
		typeof candidate.revision === "string" &&
		/^[0-9a-f]{40}$/i.test(candidate.revision) &&
		candidate.files !== undefined &&
		candidate.files !== null &&
		typeof candidate.files === "object" &&
		Object.entries(candidate.files).every(
			([path, hash]) =>
				path.length > 0 &&
				!path.startsWith("../") &&
				!path.includes("\\") &&
				typeof hash === "string" &&
				/^[0-9a-f]{64}$/i.test(hash),
		) &&
		Object.keys(candidate.files).some((path) => path.endsWith(`/${GGUF_MODEL_FILE}`))
	);
}

async function hashFile(path: string): Promise<string> {
	const hash = createHash("sha256");
	for await (const chunk of createReadStream(path)) hash.update(chunk);
	return hash.digest("hex");
}

/** Verify only local, already-downloaded semantic assets. This function never fetches. */
export async function getLocalEmbeddingAvailability(agentDir: string): Promise<LocalEmbeddingAvailability> {
	const manifestPath = getManifestPath(agentDir);
	if (!existsSync(manifestPath)) return { state: "missing" };
	let manifest: unknown;
	try {
		manifest = JSON.parse(await readFile(manifestPath, "utf8"));
	} catch {
		return { state: "invalid", reason: "asset manifest cannot be read" };
	}
	if (!isAssetManifest(manifest) || Object.keys(manifest.files).length === 0) {
		return { state: "invalid", reason: "asset manifest is invalid" };
	}
	const root = getCacheDirectory(agentDir);
	for (const [relativePath, expectedHash] of Object.entries(manifest.files)) {
		const assetPath = join(root, relativePath);
		if (!existsSync(assetPath)) return { state: "invalid", reason: `missing ${relativePath}` };
		try {
			const actualHash = await hashFile(assetPath);
			if (actualHash !== expectedHash) return { state: "invalid", reason: `checksum mismatch for ${relativePath}` };
		} catch {
			return { state: "invalid", reason: `cannot verify ${relativePath}` };
		}
	}
	return { state: "ready" };
}

interface WorkerResponse {
	id?: unknown;
	vectors?: unknown;
	error?: unknown;
	status?: unknown;
}

interface PendingRequest {
	resolve: (vectors: Float32Array[]) => void;
	reject: (error: Error) => void;
}

function isWorkerResponse(value: unknown): value is WorkerResponse {
	return value !== null && typeof value === "object";
}

function asVectors(value: unknown): Float32Array[] | undefined {
	if (!Array.isArray(value)) return undefined;
	const vectors: Float32Array[] = [];
	for (const vector of value) {
		if (!Array.isArray(vector) || !vector.every((entry) => typeof entry === "number" && Number.isFinite(entry))) {
			return undefined;
		}
		vectors.push(Float32Array.from(vector));
	}
	return vectors;
}

function asStatus(value: unknown): LocalEmbeddingStatus | undefined {
	if (!value || typeof value !== "object") return undefined;
	const candidate = value as { phase?: unknown; loadedBytes?: unknown; totalBytes?: unknown; file?: unknown };
	if (candidate.phase === "loading" || candidate.phase === "ready") return { phase: candidate.phase };
	if (candidate.phase !== "downloading") return undefined;
	return {
		phase: "downloading",
		...(typeof candidate.loadedBytes === "number" && Number.isFinite(candidate.loadedBytes)
			? { loadedBytes: candidate.loadedBytes }
			: {}),
		...(typeof candidate.totalBytes === "number" && Number.isFinite(candidate.totalBytes)
			? { totalBytes: candidate.totalBytes }
			: {}),
		...(typeof candidate.file === "string" ? { file: candidate.file } : {}),
	};
}

/** Owns the fixed local semantic-search model without exposing provider configuration. */
export class LocalEmbeddingWorker implements LocalEmbeddingEngine {
	private worker: Worker | undefined;
	private nextRequestId = 1;
	private readonly pending = new Map<number, PendingRequest>();
	private readonly agentDir: string;
	private readonly onStatus: ((status: LocalEmbeddingStatus) => void) | undefined;

	constructor(agentDir: string, options?: { onStatus?: (status: LocalEmbeddingStatus) => void }) {
		this.agentDir = agentDir;
		this.onStatus = options?.onStatus;
	}

	async prepare(): Promise<void> {
		const worker = this.ensureWorker();
		const id = this.nextRequestId++;
		await new Promise<void>((resolve, reject) => {
			this.pending.set(id, { resolve: () => resolve(), reject });
			worker.postMessage({ id, kind: "prepare", agentDir: this.agentDir });
		});
	}

	async embedQuery(query: string): Promise<Float32Array> {
		const [vector] = await this.embed("query", [query]);
		if (!vector) throw new Error("Local embedding model returned no query vector");
		return vector;
	}

	async embedDocuments(documents: readonly string[]): Promise<Float32Array[]> {
		return await this.embed("document", documents);
	}

	async dispose(): Promise<void> {
		const worker = this.worker;
		this.worker = undefined;
		for (const request of this.pending.values()) request.reject(new Error("Local embedding worker disposed"));
		this.pending.clear();
		if (worker) await worker.terminate();
	}

	private async embed(mode: "query" | "document", texts: readonly string[]): Promise<Float32Array[]> {
		if (texts.length === 0) return [];
		const worker = this.ensureWorker();
		const id = this.nextRequestId++;
		return await new Promise<Float32Array[]>((resolve, reject) => {
			this.pending.set(id, { resolve, reject });
			worker.postMessage({ id, kind: "embed", agentDir: this.agentDir, mode, texts: [...texts] });
		});
	}

	private ensureWorker(): Worker {
		if (this.worker) return this.worker;
		const isTypeScriptRuntime = import.meta.url.endsWith(".ts");
		const workerUrl = new URL(
			isTypeScriptRuntime ? "./local-embedding-worker.ts" : "./local-embedding-worker.js",
			import.meta.url,
		);
		// A worker does not support Node's eval-only --input-type flag. Filtering it
		// keeps source-mode smoke tests and normal CLI execution on the same path.
		const worker = new Worker(workerUrl, {
			execArgv: process.execArgv.filter((argument) => !argument.startsWith("--input-type")),
		});
		worker.on("message", (message: unknown) => this.handleMessage(message));
		worker.on("error", (error) => this.failPending(error));
		worker.on("exit", (code) => {
			if (code !== 0) this.failPending(new Error(`Local embedding worker exited with code ${code}`));
			if (this.worker === worker) this.worker = undefined;
		});
		this.worker = worker;
		return worker;
	}

	private handleMessage(message: unknown): void {
		if (!isWorkerResponse(message)) return;
		const status = asStatus(message.status);
		if (status) {
			this.onStatus?.(status);
			return;
		}
		if (typeof message.id !== "number") return;
		const pending = this.pending.get(message.id);
		if (!pending) return;
		this.pending.delete(message.id);
		if (typeof message.error === "string") {
			pending.reject(new Error(message.error));
			return;
		}
		const vectors = asVectors(message.vectors);
		if (!vectors) {
			pending.reject(new Error("Invalid local embedding worker response"));
			return;
		}
		pending.resolve(vectors);
	}

	private failPending(error: Error): void {
		for (const pending of this.pending.values()) pending.reject(error);
		this.pending.clear();
	}
}
