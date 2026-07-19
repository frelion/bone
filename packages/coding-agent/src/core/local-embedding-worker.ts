import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { parentPort } from "node:worker_threads";

const MODEL_ID = "cstr/multilingual-e5-small-GGUF";
const MODEL_CACHE_DIRECTORY = "bone-semantic-search-v2";
const GGUF_MODEL_FILE = "multilingual-e5-small-q8_0.gguf";
const EMBEDDING_DIMENSIONS = 384;

const require = createRequire(import.meta.url);

/**
 * Bun embeds Koffi's JavaScript but cannot embed its N-API addon. Release
 * archives place that addon beside the executable under node_modules/koffi;
 * Koffi's resource-path fallback then resolves it from there.
 */
function loadKoffi(): typeof import("koffi") {
	if (process.versions.bun) {
		const processWithResourcesPath = process as NodeJS.Process & { resourcesPath?: string };
		processWithResourcesPath.resourcesPath ??= dirname(process.execPath);
	}
	return require("koffi") as typeof import("koffi");
}

const koffi = loadKoffi();

interface ModelAssetManifest {
	format: "bone-semantic-search-assets-v2";
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

interface PrepareRequest {
	id: number;
	kind: "prepare";
	agentDir: string;
}

interface DisposeRequest {
	id: number;
	kind: "dispose";
}

type WorkerRequest = EmbedRequest | PrepareRequest | DisposeRequest;

interface WorkerResponse {
	id: number;
	vectors?: number[][];
	error?: string;
	status?: { phase: "loading" } | { phase: "ready" };
}

interface CrispEmbedFunctions {
	init: (modelPath: string, threads: number) => unknown;
	setPrefix: (context: unknown, prefix: string) => void;
	encodeBatch: (context: unknown, texts: Array<string | null>, count: number, dimensions: number[]) => unknown;
	free: (context: unknown) => void;
	unload: () => void;
}

interface NativeE5Engine {
	context: unknown;
	native: CrispEmbedFunctions;
}

let engine: NativeE5Engine | undefined;
let requestQueue: Promise<void> = Promise.resolve();

function isWorkerRequest(value: unknown): value is WorkerRequest {
	if (!value || typeof value !== "object") return false;
	const candidate = value as Partial<WorkerRequest>;
	return (
		typeof candidate.id === "number" &&
		(candidate.kind === "dispose" || candidate.kind === "embed" || candidate.kind === "prepare")
	);
}

function cacheDirectory(agentDir: string): string {
	return join(agentDir, "models", MODEL_CACHE_DIRECTORY);
}

async function readVerifiedManifest(agentDir: string): Promise<ModelAssetManifest | undefined> {
	const path = join(cacheDirectory(agentDir), "asset-manifest.json");
	if (!existsSync(path)) return undefined;
	try {
		const manifest: unknown = JSON.parse(await readFile(path, "utf8"));
		if (!manifest || typeof manifest !== "object") return undefined;
		const candidate = manifest as Partial<ModelAssetManifest>;
		if (
			candidate.format !== "bone-semantic-search-assets-v2" ||
			candidate.modelId !== MODEL_ID ||
			typeof candidate.revision !== "string" ||
			!/^[0-9a-f]{40}$/i.test(candidate.revision) ||
			!candidate.files ||
			!Object.keys(candidate.files).some((file) => file.endsWith(`/${GGUF_MODEL_FILE}`))
		) {
			return undefined;
		}
		const files = candidate.files as Record<string, unknown>;
		const fileEntries = Object.entries(files);
		if (
			!fileEntries.every(
				([file, hash]) =>
					file.length > 0 &&
					!file.startsWith("../") &&
					!file.includes("\\") &&
					typeof hash === "string" &&
					/^[0-9a-f]{64}$/i.test(hash),
			)
		)
			return undefined;
		const verifiedFiles = Object.fromEntries(fileEntries) as Record<string, string>;
		const root = cacheDirectory(agentDir);
		// The parent process has already performed the full SHA-256 verification
		// before starting this worker. Avoid a second complete read of the GGUF at
		// startup; this worker only validates the manifest structure and paths.
		for (const file of Object.keys(verifiedFiles)) {
			const filePath = join(root, file);
			if (!existsSync(filePath)) return undefined;
		}
		return {
			format: "bone-semantic-search-assets-v2",
			modelId: MODEL_ID,
			revision: candidate.revision,
			files: verifiedFiles,
		};
	} catch {
		return undefined;
	}
}

function postStatus(status: NonNullable<WorkerResponse["status"]>): void {
	parentPort?.postMessage({ id: 0, status } satisfies WorkerResponse);
}

function resolveNativeLibraryPath(): string {
	const platform = `${process.platform}-${process.arch}`;
	const libraryName =
		platform === "darwin-arm64" || platform === "darwin-x64"
			? "libcrispembed.0.dylib"
			: platform === "linux-x64" || platform === "linux-arm64"
				? "libcrispembed.so.0"
				: platform === "win32-x64" || platform === "win32-arm64"
					? "crispembed.dll"
					: undefined;
	if (!libraryName) {
		throw new Error(`Local GGUF semantic search is not bundled for ${platform}`);
	}
	const sourceRuntime = import.meta.url.endsWith(".ts");
	const moduleRelativePath = fileURLToPath(
		new URL(
			sourceRuntime ? `../../native/${platform}/${libraryName}` : `../native/${platform}/${libraryName}`,
			import.meta.url,
		),
	);
	if (existsSync(moduleRelativePath)) return moduleRelativePath;
	// Bun's compiled filesystem has no ordinary module-relative path. The binary
	// release places the sidecars beside the executable instead.
	const binaryRelativePath = join(dirname(process.execPath), "native", platform, libraryName);
	if (existsSync(binaryRelativePath)) return binaryRelativePath;
	throw new Error("Bone's local GGUF embedding engine is missing from this installation");
}

function loadCrispEmbed(): CrispEmbedFunctions {
	const library = koffi.load(resolveNativeLibraryPath());
	return {
		init: library.func("void * crispembed_init(const char * model_path, int n_threads)"),
		setPrefix: library.func("void crispembed_set_prefix(void * context, const char * prefix)"),
		encodeBatch: library.func(
			"const float * crispembed_encode_batch(void * context, const char ** texts, int n_texts, _Out_ int * out_n_dim)",
		),
		free: library.func("void crispembed_free(void * context)"),
		unload: () => library.unload(),
	};
}

async function loadNativeEngine(agentDir: string): Promise<NativeE5Engine> {
	if (engine) return engine;
	const manifest = await readVerifiedManifest(agentDir);
	if (!manifest) throw new Error("Local semantic model assets are not prepared. Run bone setup.");
	const modelPath = join(cacheDirectory(agentDir), MODEL_ID, manifest.revision, GGUF_MODEL_FILE);
	if (!existsSync(modelPath)) throw new Error("Local semantic model is missing its Q8 GGUF model");
	postStatus({ phase: "loading" });
	// The native engine uses a fixed CPU-only single-thread configuration. It keeps
	// the GGUF's weights as a read-only MAP_SHARED mapping instead of making an
	// ORT/JS heap copy of the model.
	process.env.CRISPEMBED_FORCE_CPU = "1";
	const native = loadCrispEmbed();
	const context = native.init(modelPath, 1);
	if (!context) {
		native.unload();
		throw new Error("Could not initialize Bone's local GGUF embedding engine");
	}
	engine = { context, native };
	postStatus({ phase: "ready" });
	return engine;
}

function decodeVectors(pointer: unknown, dimensions: number, count: number): number[][] {
	if (!pointer) throw new Error("Local GGUF embedding engine returned no vectors");
	if (dimensions !== EMBEDDING_DIMENSIONS) {
		throw new Error(
			`Local GGUF embedding engine returned ${dimensions} dimensions; expected ${EMBEDDING_DIMENSIONS}`,
		);
	}
	const data = koffi.decode(pointer, "float", dimensions * count) as Float32Array;
	if (!(data instanceof Float32Array) || data.length !== dimensions * count) {
		throw new Error("Local GGUF embedding engine returned an invalid vector buffer");
	}
	return Array.from({ length: count }, (_, index) =>
		Array.from(data.subarray(index * dimensions, (index + 1) * dimensions)),
	);
}

async function embed(request: EmbedRequest): Promise<number[][]> {
	const native = await loadNativeEngine(request.agentDir);
	native.native.setPrefix(
		native.context,
		request.mode === "query" ? "query: Find the previous Bone conversation relevant to: " : "passage: ",
	);
	const dimensions = [0];
	const pointer = native.native.encodeBatch(native.context, request.texts, request.texts.length, dimensions);
	return decodeVectors(pointer, dimensions[0] ?? 0, request.texts.length);
}

async function dispose(): Promise<void> {
	const current = engine;
	engine = undefined;
	if (!current) return;
	current.native.free(current.context);
	current.native.unload();
}

function enqueue(task: () => Promise<void>): void {
	requestQueue = requestQueue.then(task, task);
}

parentPort?.on("message", (message: unknown) => {
	if (!isWorkerRequest(message)) return;
	enqueue(async () => {
		try {
			if (message.kind === "dispose") {
				await dispose();
				parentPort?.postMessage({ id: message.id, vectors: [] } satisfies WorkerResponse);
				return;
			}
			if (message.kind === "prepare") {
				await loadNativeEngine(message.agentDir);
				parentPort?.postMessage({ id: message.id, vectors: [] } satisfies WorkerResponse);
				return;
			}
			parentPort?.postMessage({ id: message.id, vectors: await embed(message) } satisfies WorkerResponse);
		} catch (error) {
			parentPort?.postMessage({
				id: message.id,
				error: error instanceof Error ? error.message : String(error),
			} satisfies WorkerResponse);
		}
	});
});
