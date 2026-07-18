import { Worker } from "node:worker_threads";

const IDLE_WORKER_TIMEOUT_MS = 5 * 60_000;

export type LocalEmbeddingStatus =
	| { phase: "downloading"; loadedBytes?: number; totalBytes?: number; file?: string }
	| { phase: "loading" }
	| { phase: "ready" };

/** Minimal boundary used by search so tests never need to start a native ONNX runtime. */
export interface LocalEmbeddingEngine {
	embedQuery(query: string): Promise<Float32Array>;
	embedDocuments(documents: readonly string[]): Promise<Float32Array[]>;
	dispose(): Promise<void>;
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
	private idleTimer: NodeJS.Timeout | undefined;
	private readonly agentDir: string;
	private readonly onStatus: ((status: LocalEmbeddingStatus) => void) | undefined;

	constructor(agentDir: string, options?: { onStatus?: (status: LocalEmbeddingStatus) => void }) {
		this.agentDir = agentDir;
		this.onStatus = options?.onStatus;
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
		if (this.idleTimer) clearTimeout(this.idleTimer);
		this.idleTimer = undefined;
		const worker = this.worker;
		this.worker = undefined;
		for (const request of this.pending.values()) request.reject(new Error("Local embedding worker disposed"));
		this.pending.clear();
		if (worker) await worker.terminate();
	}

	private async embed(mode: "query" | "document", texts: readonly string[]): Promise<Float32Array[]> {
		if (texts.length === 0) return [];
		const worker = this.ensureWorker();
		if (this.idleTimer) clearTimeout(this.idleTimer);
		this.idleTimer = undefined;
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
		this.scheduleIdleDispose();
	}

	private failPending(error: Error): void {
		for (const pending of this.pending.values()) pending.reject(error);
		this.pending.clear();
	}

	private scheduleIdleDispose(): void {
		if (this.pending.size > 0) return;
		if (this.idleTimer) clearTimeout(this.idleTimer);
		this.idleTimer = setTimeout(() => void this.dispose(), IDLE_WORKER_TIMEOUT_MS);
	}
}
