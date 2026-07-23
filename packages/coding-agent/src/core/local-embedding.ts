import { createHash } from "node:crypto";
import { createReadStream, existsSync } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
	getLocalEmbeddingCacheDirectory,
	getLocalEmbeddingManifestPath,
	isLocalEmbeddingAssetManifest,
	LOCAL_EMBEDDING_DIMENSIONS,
	LOCAL_EMBEDDING_GGUF_FILE,
	LOCAL_EMBEDDING_MODEL_ID,
	type LocalEmbeddingAssetManifest,
} from "./local-embedding-assets.ts";

export type LocalEmbeddingAvailability =
	| { state: "ready" }
	| { state: "missing" }
	| { state: "invalid"; reason: string };

export type LocalEmbeddingStatus =
	| { phase: "downloading"; loadedBytes?: number; totalBytes?: number; file?: string }
	| { phase: "loading" }
	| { phase: "ready" };

export type LocalEmbeddingEnginePhase = "not-started" | "loading" | "ready" | "embedding" | "failed" | "disposed";

export interface LocalEmbeddingEngineDiagnostics {
	phase: LocalEmbeddingEnginePhase;
	runtime: "same-process-worker-thread";
	pendingQueries: number;
	pendingDocuments: number;
	activeDocuments: number;
	pid?: number;
	modelFingerprint?: string;
	error?: string;
}

interface BunWorker {
	onmessage: ((event: { data: unknown }) => void) | null;
	onerror: ((event: { message: string }) => void) | null;
	postMessage(message: unknown): void;
	terminate(): void;
}

declare const Worker: {
	new (specifier: string | URL, options: { type: "module" }): BunWorker;
};

/** Minimal boundary used by search so tests never need to start the native GGUF runtime. */
export interface LocalEmbeddingEngine {
	/** Resolve and verify the fixed local model before any search query needs it. */
	prepare(): Promise<void>;
	embedQuery(query: string): Promise<Float32Array>;
	embedDocuments(documents: readonly string[]): Promise<Float32Array[]>;
	dispose(): Promise<void>;
	/** Optional because tests may substitute a minimal in-memory embedding engine. */
	getDiagnostics?(): LocalEmbeddingEngineDiagnostics;
}

type PendingRequest = {
	kind: "prepare" | "embed" | "dispose";
	expectedCount: number;
	resolve: (vectors: Float32Array[]) => void;
	reject: (error: Error) => void;
};

type WorkerStatus = {
	phase?: unknown;
	pendingQueries?: unknown;
	pendingDocuments?: unknown;
	activeDocuments?: unknown;
	pid?: unknown;
	modelFingerprint?: unknown;
	error?: unknown;
};

function initialDiagnostics(): LocalEmbeddingEngineDiagnostics {
	return {
		phase: "not-started",
		runtime: "same-process-worker-thread",
		pendingQueries: 0,
		pendingDocuments: 0,
		activeDocuments: 0,
	};
}

async function hashFile(path: string): Promise<string> {
	const hash = createHash("sha256");
	for await (const chunk of createReadStream(path)) hash.update(chunk);
	return hash.digest("hex");
}

interface VerifiedManifestCacheEntry {
	manifestSource: string;
	assetSignature: string;
	manifest: LocalEmbeddingAssetManifest;
}

type ManifestVerification =
	| { state: "ready"; manifest: LocalEmbeddingAssetManifest }
	| { state: "missing" }
	| { state: "invalid"; reason: string };

const verifiedManifestCache = new Map<string, VerifiedManifestCacheEntry>();

async function verifyLocalEmbeddingManifest(agentDir: string): Promise<ManifestVerification> {
	const manifestPath = getLocalEmbeddingManifestPath(agentDir);
	if (!existsSync(manifestPath)) return { state: "missing" };

	let manifestSource: string;
	let manifest: unknown;
	try {
		manifestSource = await readFile(manifestPath, "utf8");
		manifest = JSON.parse(manifestSource);
	} catch {
		verifiedManifestCache.delete(manifestPath);
		return { state: "invalid", reason: "asset manifest cannot be read" };
	}
	if (!isLocalEmbeddingAssetManifest(manifest) || Object.keys(manifest.files).length === 0) {
		verifiedManifestCache.delete(manifestPath);
		return { state: "invalid", reason: "asset manifest is invalid" };
	}

	const root = getLocalEmbeddingCacheDirectory(agentDir);
	const assets: Array<{ path: string; expectedHash: string; signature: string }> = [];
	for (const [relativePath, expectedHash] of Object.entries(manifest.files).sort(([left], [right]) =>
		left.localeCompare(right),
	)) {
		const assetPath = join(root, relativePath);
		try {
			const metadata = await stat(assetPath);
			if (!metadata.isFile()) throw new Error("not a file");
			assets.push({
				path: assetPath,
				expectedHash,
				signature: `${relativePath}:${metadata.size}:${metadata.mtimeMs}:${metadata.ctimeMs}`,
			});
		} catch {
			verifiedManifestCache.delete(manifestPath);
			return { state: "invalid", reason: `missing ${relativePath}` };
		}
	}

	const assetSignature = assets.map((asset) => asset.signature).join("|");
	const cached = verifiedManifestCache.get(manifestPath);
	if (cached?.manifestSource === manifestSource && cached.assetSignature === assetSignature) {
		return { state: "ready", manifest: cached.manifest };
	}

	for (const asset of assets) {
		try {
			if ((await hashFile(asset.path)) !== asset.expectedHash) {
				verifiedManifestCache.delete(manifestPath);
				return { state: "invalid", reason: `checksum mismatch for ${asset.path.slice(root.length + 1)}` };
			}
		} catch {
			verifiedManifestCache.delete(manifestPath);
			return { state: "invalid", reason: `cannot verify ${asset.path.slice(root.length + 1)}` };
		}
	}
	verifiedManifestCache.set(manifestPath, { manifestSource, assetSignature, manifest });
	return { state: "ready", manifest };
}

/** Verify only local, already-downloaded semantic assets. This function never fetches. */
export async function getLocalEmbeddingAvailability(agentDir: string): Promise<LocalEmbeddingAvailability> {
	const verification = await verifyLocalEmbeddingManifest(agentDir);
	return verification.state === "ready" ? { state: "ready" } : verification;
}

function nativePlatform(): string | undefined {
	const platform = `${process.platform}-${process.arch}`;
	return new Set(["darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64", "win32-x64", "win32-arm64"]).has(platform)
		? platform
		: undefined;
}

function resolveNativeLibrary(): string {
	const platform = nativePlatform();
	if (!platform) throw new Error(`Local GGUF semantic search is not bundled for ${process.platform}-${process.arch}`);
	const moduleDirectory = dirname(fileURLToPath(import.meta.url));
	const sourceRuntime = import.meta.url.endsWith(".ts");
	const candidates = [
		sourceRuntime
			? join(moduleDirectory, "..", "..", "native", platform)
			: join(moduleDirectory, "..", "native", platform),
		join(dirname(process.execPath), "native", platform),
	];
	const libraryName =
		process.platform === "darwin"
			? "libcrispembed.0.dylib"
			: process.platform === "win32"
				? "crispembed.dll"
				: "libcrispembed.so.0";
	for (const directory of candidates) {
		const candidate = join(directory, libraryName);
		if (existsSync(candidate)) return candidate;
	}
	throw new Error("Bone's local GGUF embedding library is missing from this installation");
}

function workerSpecifier(): URL {
	return new URL(
		import.meta.url.endsWith(".ts") ? "./local-embedding-worker.ts" : "./local-embedding-worker.js",
		import.meta.url,
	);
}

function localEmbeddingWorkerEntry(): string | URL {
	// Bun standalone executables resolve explicitly bundled worker entrypoints
	// by their source path. URL resolution points into the executable's virtual
	// filesystem instead, leaving the worker alive but unable to receive work.
	if (typeof process.versions.bun === "string" && !import.meta.url.startsWith("file:")) {
		return "./src/core/local-embedding-worker.ts";
	}
	return workerSpecifier();
}

function asDiagnostics(status: WorkerStatus): LocalEmbeddingEngineDiagnostics | undefined {
	const phase = status.phase;
	if (
		phase !== "not-started" &&
		phase !== "loading" &&
		phase !== "ready" &&
		phase !== "embedding" &&
		phase !== "failed" &&
		phase !== "disposed"
	)
		return undefined;
	const number = (value: unknown): number =>
		typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : 0;
	return {
		phase,
		runtime: "same-process-worker-thread",
		pendingQueries: number(status.pendingQueries),
		pendingDocuments: number(status.pendingDocuments),
		activeDocuments: number(status.activeDocuments),
		...(typeof status.pid === "number" && Number.isSafeInteger(status.pid) ? { pid: status.pid } : {}),
		...(typeof status.modelFingerprint === "string" ? { modelFingerprint: status.modelFingerprint } : {}),
		...(typeof status.error === "string" ? { error: status.error } : {}),
	};
}

/**
 * Owns one Bun Worker in the same Bone process. The worker is the only thread
 * allowed to access CrispEmbed's context through Bun FFI.
 */
export class LocalEmbeddingWorker implements LocalEmbeddingEngine {
	private worker: BunWorker | undefined;
	private nextRequestId = 1;
	private readonly pending = new Map<number, PendingRequest>();
	private preparePromise: Promise<void> | undefined;
	private prepared = false;
	private readonly agentDir: string;
	private readonly onStatus: ((status: LocalEmbeddingStatus) => void) | undefined;
	private diagnostics = initialDiagnostics();

	constructor(agentDir: string, options?: { onStatus?: (status: LocalEmbeddingStatus) => void }) {
		this.agentDir = agentDir;
		this.onStatus = options?.onStatus;
	}

	async prepare(): Promise<void> {
		if (this.prepared) return;
		if (!this.preparePromise) {
			this.preparePromise = this.prepareInternal().catch((error: unknown) => {
				this.preparePromise = undefined;
				throw error;
			});
		}
		await this.preparePromise;
	}

	getDiagnostics(): LocalEmbeddingEngineDiagnostics {
		return { ...this.diagnostics };
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
		this.prepared = false;
		this.preparePromise = undefined;
		if (!worker) {
			this.diagnostics = { ...this.diagnostics, phase: "disposed" };
			return;
		}
		try {
			await this.request("dispose", 0, {});
		} catch {
			// The Worker may already have exited. Termination still releases its JS state.
		}
		worker.terminate();
		if (this.worker === worker) this.worker = undefined;
		this.diagnostics = {
			...this.diagnostics,
			phase: "disposed",
			pendingQueries: 0,
			pendingDocuments: 0,
			activeDocuments: 0,
		};
		this.failPending(new Error("Local embedding worker disposed"));
	}

	private async prepareInternal(): Promise<void> {
		const verification = await verifyLocalEmbeddingManifest(this.agentDir);
		if (verification.state !== "ready")
			throw new Error("Local semantic model assets are not prepared. Run bone setup.");
		const modelPath = join(
			getLocalEmbeddingCacheDirectory(this.agentDir),
			LOCAL_EMBEDDING_MODEL_ID,
			verification.manifest.revision,
			LOCAL_EMBEDDING_GGUF_FILE,
		);
		if (!existsSync(modelPath)) throw new Error("Local semantic model is missing its Q8 GGUF model");
		this.diagnostics = { ...this.diagnostics, phase: "loading" };
		this.onStatus?.({ phase: "loading" });
		await this.request("prepare", 0, {
			modelPath,
			libraryPath: resolveNativeLibrary(),
			fingerprint: `${verification.manifest.modelId}@${verification.manifest.revision}`,
		});
		this.prepared = true;
		this.onStatus?.({ phase: "ready" });
	}

	private async embed(mode: "query" | "document", texts: readonly string[]): Promise<Float32Array[]> {
		if (texts.length === 0) return [];
		await this.prepare();
		return await this.request("embed", texts.length, { mode, texts: [...texts] });
	}

	private ensureWorker(): BunWorker {
		if (this.worker) return this.worker;
		const worker = new Worker(localEmbeddingWorkerEntry(), { type: "module" });
		worker.onmessage = (event) => this.handleMessage(event.data);
		worker.onerror = (event) => this.handleWorkerFailure(new Error(event.message));
		this.worker = worker;
		return worker;
	}

	private async request(
		kind: PendingRequest["kind"],
		expectedCount: number,
		payload: Record<string, unknown>,
	): Promise<Float32Array[]> {
		const worker = this.ensureWorker();
		const id = this.nextRequestId++;
		return await new Promise<Float32Array[]>((resolve, reject) => {
			this.pending.set(id, { kind, expectedCount, resolve, reject });
			worker.postMessage({ type: kind, id, ...payload });
		});
	}

	private handleMessage(message: unknown): void {
		if (!message || typeof message !== "object" || !("type" in message) || typeof message.type !== "string") return;
		const candidate = message as {
			type: string;
			id?: unknown;
			status?: unknown;
			vectors?: unknown;
			message?: unknown;
		};
		if (candidate.type === "status") {
			const status = asDiagnostics((candidate.status ?? {}) as WorkerStatus);
			if (status) this.diagnostics = status;
			return;
		}
		if (typeof candidate.id !== "number") return;
		const pending = this.pending.get(candidate.id);
		if (!pending) return;
		this.pending.delete(candidate.id);
		if (candidate.type === "error") {
			pending.reject(
				new Error(typeof candidate.message === "string" ? candidate.message : "Local embedding worker failed"),
			);
			return;
		}
		if (candidate.type === "result") {
			if (pending.kind === "embed") pending.reject(new Error("Local embedding worker returned no vectors"));
			else pending.resolve([]);
			return;
		}
		if (candidate.type !== "vectors" || pending.kind !== "embed" || !Array.isArray(candidate.vectors)) {
			pending.reject(new Error("Local embedding worker returned an invalid response"));
			return;
		}
		const vectors = candidate.vectors.filter((vector): vector is Float32Array => vector instanceof Float32Array);
		if (
			vectors.length !== pending.expectedCount ||
			vectors.some((vector) => vector.length !== LOCAL_EMBEDDING_DIMENSIONS)
		) {
			pending.reject(new Error("Local embedding worker returned an unexpected vector shape"));
			return;
		}
		pending.resolve(vectors);
	}

	private handleWorkerFailure(error: Error): void {
		this.prepared = false;
		this.diagnostics = { ...this.diagnostics, phase: "failed", error: error.message };
		this.failPending(error);
	}

	private failPending(error: Error): void {
		for (const pending of this.pending.values()) pending.reject(error);
		this.pending.clear();
	}
}
