import { type ChildProcessWithoutNullStreams, spawn } from "node:child_process";
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

const MAX_SIDECAR_FRAME_BYTES = 128 * 1024 * 1024;

const SIDECAR_PREPARE = 1;
const SIDECAR_EMBED = 2;
const SIDECAR_DISPOSE = 3;
const SIDECAR_SUCCESS = 128;
const SIDECAR_ERROR = 129;

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

interface PendingRequest {
	kind: "prepare" | "embed" | "dispose";
	expectedCount: number;
	resolve: (vectors: Float32Array[]) => void;
	reject: (error: Error) => void;
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

// A startup checks availability first and then starts this worker. Reusing a
// verified manifest prevents reading the Q8 GGUF twice while still rechecking
// the manifest and every asset's metadata before trusting the cache.
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

function uint32(value: number): Buffer {
	const buffer = Buffer.allocUnsafe(4);
	buffer.writeUInt32LE(value, 0);
	return buffer;
}

function stringFrame(value: string): Buffer {
	const encoded = Buffer.from(value, "utf8");
	return Buffer.concat([uint32(encoded.length), encoded]);
}

function sidecarExecutableName(): string | undefined {
	const platform = `${process.platform}-${process.arch}`;
	if (!new Set(["darwin-arm64", "darwin-x64", "linux-x64", "linux-arm64", "win32-x64", "win32-arm64"]).has(platform)) {
		return undefined;
	}
	return process.platform === "win32" ? "bone-embed.exe" : "bone-embed";
}

function resolveSidecarExecutable(): string {
	const executable = sidecarExecutableName();
	const platform = `${process.platform}-${process.arch}`;
	if (!executable) throw new Error(`Local GGUF semantic search is not bundled for ${platform}`);
	const moduleDirectory = dirname(fileURLToPath(import.meta.url));
	const sourceRuntime = import.meta.url.endsWith(".ts");
	const candidates = [
		sourceRuntime
			? join(moduleDirectory, "..", "..", "native", platform, executable)
			: join(moduleDirectory, "..", "native", platform, executable),
		join(dirname(process.execPath), "native", platform, executable),
	];
	for (const candidate of candidates) {
		if (existsSync(candidate)) return candidate;
	}
	throw new Error("Bone's local GGUF embedding sidecar is missing from this installation");
}

/**
 * Owns a private native child process. It contains the model and its mmap'd
 * weights, while the Node process retains only TUI/search orchestration state.
 */
export class LocalEmbeddingWorker implements LocalEmbeddingEngine {
	private sidecar: ChildProcessWithoutNullStreams | undefined;
	private nextRequestId = 1;
	private readonly pending = new Map<number, PendingRequest>();
	private output = Buffer.alloc(0);
	private stderr = "";
	private preparePromise: Promise<void> | undefined;
	private prepared = false;
	private readonly agentDir: string;
	private readonly onStatus: ((status: LocalEmbeddingStatus) => void) | undefined;

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

	async embedQuery(query: string): Promise<Float32Array> {
		const [vector] = await this.embed("query", [query]);
		if (!vector) throw new Error("Local embedding model returned no query vector");
		return vector;
	}

	async embedDocuments(documents: readonly string[]): Promise<Float32Array[]> {
		return await this.embed("document", documents);
	}

	async dispose(): Promise<void> {
		const sidecar = this.sidecar;
		this.prepared = false;
		this.preparePromise = undefined;
		if (!sidecar) return;
		try {
			await this.sendRequest("dispose", 0, Buffer.alloc(0));
		} catch {
			// The process may already have exited; it is still safe to finish disposal.
		}
		sidecar.stdin.end();
		await new Promise<void>((resolve) => {
			const timeout = setTimeout(() => {
				if (!sidecar.killed) sidecar.kill();
				resolve();
			}, 1_000);
			sidecar.once("exit", () => {
				clearTimeout(timeout);
				resolve();
			});
		});
		if (this.sidecar === sidecar) this.sidecar = undefined;
		this.failPending(new Error("Local embedding sidecar disposed"));
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
		this.onStatus?.({ phase: "loading" });
		this.ensureSidecar();
		await this.sendRequest("prepare", 0, stringFrame(modelPath));
		this.prepared = true;
		this.onStatus?.({ phase: "ready" });
	}

	private async embed(mode: "query" | "document", texts: readonly string[]): Promise<Float32Array[]> {
		if (texts.length === 0) return [];
		await this.prepare();
		const payload = Buffer.concat([
			Buffer.from([mode === "query" ? 0 : 1]),
			uint32(texts.length),
			...texts.map((text) => stringFrame(text)),
		]);
		return await this.sendRequest("embed", texts.length, payload);
	}

	private ensureSidecar(): ChildProcessWithoutNullStreams {
		if (this.sidecar) return this.sidecar;
		const sidecar = spawn(resolveSidecarExecutable(), [], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
		sidecar.stdout.on("data", (chunk: Buffer) => this.handleOutput(chunk));
		sidecar.stderr.on("data", (chunk: Buffer) => {
			this.stderr = `${this.stderr}${chunk.toString("utf8")}`.slice(-8_192);
		});
		sidecar.on("error", (error) => this.failPending(error));
		sidecar.on("exit", (code, signal) => {
			if (this.sidecar === sidecar) this.sidecar = undefined;
			if (this.pending.size > 0) {
				const detail = this.stderr.trim();
				this.failPending(
					new Error(
						`Local embedding sidecar exited (${signal ?? code ?? "unknown"})${detail ? `: ${detail}` : ""}`,
					),
				);
			}
		});
		this.sidecar = sidecar;
		return sidecar;
	}

	private async sendRequest(
		kind: PendingRequest["kind"],
		expectedCount: number,
		payload: Buffer,
	): Promise<Float32Array[]> {
		const sidecar = this.sidecar ?? this.ensureSidecar();
		const id = this.nextRequestId++;
		const type = kind === "prepare" ? SIDECAR_PREPARE : kind === "embed" ? SIDECAR_EMBED : SIDECAR_DISPOSE;
		const frame = Buffer.concat([Buffer.from([type]), uint32(id), payload]);
		return await new Promise<Float32Array[]>((resolve, reject) => {
			this.pending.set(id, { kind, expectedCount, resolve, reject });
			sidecar.stdin.write(frame, (error) => {
				if (!error) return;
				const pending = this.pending.get(id);
				if (!pending) return;
				this.pending.delete(id);
				pending.reject(error);
			});
		});
	}

	private handleOutput(chunk: Buffer): void {
		this.output = Buffer.concat([this.output, chunk]);
		if (this.output.length > MAX_SIDECAR_FRAME_BYTES) {
			this.failPending(new Error("Local embedding sidecar emitted an oversized response"));
			this.sidecar?.kill();
			return;
		}
		while (this.output.length >= 5) {
			const type = this.output.readUInt8(0);
			const id = this.output.readUInt32LE(1);
			if (type === SIDECAR_SUCCESS) {
				if (this.output.length < 13) return;
				const count = this.output.readUInt32LE(5);
				const dimensions = this.output.readUInt32LE(9);
				const vectorBytes = count * dimensions * Float32Array.BYTES_PER_ELEMENT;
				const frameBytes = 13 + vectorBytes;
				if (!Number.isSafeInteger(frameBytes) || frameBytes > MAX_SIDECAR_FRAME_BYTES) {
					this.failPending(new Error("Local embedding sidecar emitted an invalid response size"));
					this.sidecar?.kill();
					return;
				}
				if (this.output.length < frameBytes) return;
				const frame = this.output.subarray(0, frameBytes);
				this.output = this.output.subarray(frameBytes);
				this.resolveSuccess(id, count, dimensions, frame.subarray(13));
				continue;
			}
			if (type === SIDECAR_ERROR) {
				if (this.output.length < 9) return;
				const messageBytes = this.output.readUInt32LE(5);
				const frameBytes = 9 + messageBytes;
				if (frameBytes > MAX_SIDECAR_FRAME_BYTES) {
					this.failPending(new Error("Local embedding sidecar emitted an invalid error size"));
					this.sidecar?.kill();
					return;
				}
				if (this.output.length < frameBytes) return;
				const message = this.output.toString("utf8", 9, frameBytes);
				this.output = this.output.subarray(frameBytes);
				this.rejectRequest(id, new Error(message || "Local embedding sidecar failed"));
				continue;
			}
			this.failPending(new Error("Local embedding sidecar emitted an unknown response"));
			this.sidecar?.kill();
			return;
		}
	}

	private resolveSuccess(id: number, count: number, dimensions: number, bytes: Buffer): void {
		const pending = this.pending.get(id);
		if (!pending) return;
		this.pending.delete(id);
		if (pending.kind !== "embed") {
			if (count !== 0 || dimensions !== 0 || bytes.length !== 0) {
				pending.reject(new Error("Local embedding sidecar returned an invalid control response"));
				return;
			}
			pending.resolve([]);
			return;
		}
		if (count !== pending.expectedCount || dimensions !== LOCAL_EMBEDDING_DIMENSIONS) {
			pending.reject(new Error("Local embedding sidecar returned an unexpected vector shape"));
			return;
		}
		const vectors: Float32Array[] = [];
		for (let item = 0; item < count; item++) {
			const vector = new Float32Array(dimensions);
			for (let dimension = 0; dimension < dimensions; dimension++) {
				vector[dimension] = bytes.readFloatLE((item * dimensions + dimension) * Float32Array.BYTES_PER_ELEMENT);
			}
			vectors.push(vector);
		}
		pending.resolve(vectors);
	}

	private rejectRequest(id: number, error: Error): void {
		const pending = this.pending.get(id);
		if (!pending) return;
		this.pending.delete(id);
		pending.reject(error);
	}

	private failPending(error: Error): void {
		for (const pending of this.pending.values()) pending.reject(error);
		this.pending.clear();
	}
}
