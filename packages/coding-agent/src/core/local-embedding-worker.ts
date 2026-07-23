import { BunFfiEmbeddingLibrary } from "./local-embedding-ffi.ts";

const DIMENSIONS = 384;

type EmbedMode = "query" | "document";

type PrepareRequest = { type: "prepare"; id: number; modelPath: string; fingerprint: string; libraryPath: string };
type EmbedRequest = { type: "embed"; id: number; mode: EmbedMode; texts: string[] };
type DisposeRequest = { type: "dispose"; id: number };
type Request = PrepareRequest | EmbedRequest | DisposeRequest;

type WorkerStatus = {
	phase: "not-started" | "loading" | "ready" | "embedding" | "failed" | "disposed";
	pendingQueries: number;
	pendingDocuments: number;
	activeDocuments: number;
	modelFingerprint?: string;
	error?: string;
	pid: number;
};

interface BunWorkerPort {
	postMessage(message: unknown, transfer?: ArrayBuffer[]): void;
	onmessage: ((event: { data: unknown }) => void) | null;
}

const port = globalThis as unknown as BunWorkerPort;

let library: BunFfiEmbeddingLibrary | undefined;
let fingerprint: string | undefined;
let phase: WorkerStatus["phase"] = "not-started";
let activeDocuments = 0;
let draining = false;
let scheduled = false;
let closing = false;
const queryQueue: EmbedRequest[] = [];
const documentQueue: EmbedRequest[] = [];

function messageError(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function publishStatus(error?: string): void {
	port.postMessage({
		type: "status",
		status: {
			phase,
			pendingQueries: queryQueue.length,
			pendingDocuments: documentQueue.length,
			activeDocuments,
			...(fingerprint ? { modelFingerprint: fingerprint } : {}),
			...(error ? { error } : {}),
			pid: process.pid,
		} satisfies WorkerStatus,
	});
}

function reject(request: Request, error: string): void {
	port.postMessage({ type: "error", id: request.id, message: error });
}

function schedule(): void {
	if (scheduled || draining || closing) return;
	scheduled = true;
	setImmediate(() => {
		scheduled = false;
		void drain();
	});
}

async function drain(): Promise<void> {
	if (draining || closing) return;
	const request = queryQueue.shift() ?? documentQueue.shift();
	if (!request) {
		if (phase !== "failed" && phase !== "disposed") phase = library ? "ready" : "not-started";
		publishStatus();
		return;
	}
	draining = true;
	activeDocuments = request.mode === "document" ? request.texts.length : 0;
	phase = "embedding";
	publishStatus();
	try {
		if (!library) throw new Error("Local semantic engine is not prepared");
		const flat = library.embed(request.mode === "query" ? 0 : 1, request.texts);
		if (flat.length !== request.texts.length * DIMENSIONS) {
			throw new Error("Local semantic engine returned an unexpected vector shape");
		}
		const vectors = Array.from({ length: request.texts.length }, (_, index) =>
			flat.slice(index * DIMENSIONS, (index + 1) * DIMENSIONS),
		);
		port.postMessage(
			{ type: "vectors", id: request.id, vectors },
			vectors.map((vector) => vector.buffer),
		);
	} catch (error) {
		const message = messageError(error);
		phase = "failed";
		reject(request, message);
		publishStatus(message);
	} finally {
		activeDocuments = 0;
		draining = false;
		if (phase !== "failed") phase = library ? "ready" : "not-started";
		publishStatus();
		schedule();
	}
}

port.onmessage = (event) => {
	const message = event.data;
	if (!message || typeof message !== "object" || !("type" in message) || typeof message.type !== "string") return;
	const request = message as Request;
	if (request.type === "prepare") {
		if (
			typeof request.id !== "number" ||
			typeof request.modelPath !== "string" ||
			typeof request.fingerprint !== "string"
		)
			return;
		if (typeof request.libraryPath !== "string") return reject(request, "Local semantic library path is missing");
		if (closing) return reject(request, "Local semantic engine is closing");
		phase = "loading";
		publishStatus();
		try {
			library ??= new BunFfiEmbeddingLibrary(request.libraryPath);
			library.prepare(request.modelPath);
			fingerprint = request.fingerprint;
			phase = "ready";
			port.postMessage({ type: "result", id: request.id });
			publishStatus();
		} catch (error) {
			phase = "failed";
			const detail = messageError(error);
			reject(request, detail);
			publishStatus(detail);
		}
		return;
	}
	if (request.type === "embed") {
		if (
			typeof request.id !== "number" ||
			(request.mode !== "query" && request.mode !== "document") ||
			!Array.isArray(request.texts)
		)
			return;
		if (closing) return reject(request, "Local semantic engine is closing");
		(request.mode === "query" ? queryQueue : documentQueue).push(request);
		publishStatus();
		schedule();
		return;
	}
	if (request.type !== "dispose" || typeof request.id !== "number") return;
	closing = true;
	for (const pending of [...queryQueue.splice(0), ...documentQueue.splice(0)])
		reject(pending, "Local semantic engine disposed");
	try {
		library?.close();
		library = undefined;
		fingerprint = undefined;
		phase = "disposed";
		port.postMessage({ type: "result", id: request.id });
		publishStatus();
	} catch (error) {
		const detail = messageError(error);
		phase = "failed";
		reject(request, detail);
		publishStatus(detail);
	}
};

publishStatus();
