import { CString, dlopen, JSCallback, type Pointer, ptr, toArrayBuffer } from "bun:ffi";

const DIMENSIONS = 384;
const MAX_TEXT_COUNT = 8192;
const MAX_TEXT_BYTES = 1024 * 1024;
const MAX_DIAGNOSTIC_BYTES = 4096;

const symbols = {
	crispembed_init: { args: ["ptr", "i32"], returns: "ptr" },
	crispembed_free: { args: ["ptr"], returns: "void" },
	crispembed_set_prefix: { args: ["ptr", "ptr"], returns: "void" },
	crispembed_encode_batch: { args: ["ptr", "ptr", "i32", "ptr"], returns: "ptr" },
	crispembed_set_log_callback: { args: ["function", "ptr"], returns: "void" },
} as const;

type CrispEmbedLibrary = {
	symbols: {
		crispembed_init(modelPath: Pointer, threads: number): Pointer | null;
		crispembed_free(context: Pointer): void;
		crispembed_set_prefix(context: Pointer, prefix: Pointer): void;
		crispembed_encode_batch(context: Pointer, texts: Pointer, count: number, dimensions: Pointer): Pointer | null;
		crispembed_set_log_callback(callback: Pointer, userData: Pointer | null): void;
	};
	close(): void;
};

function cString(value: string): Uint8Array {
	return Buffer.from(`${value}\0`, "utf8");
}

/** Bun-owned FFI boundary for the fixed CrispEmbed C ABI. */
export class BunFfiEmbeddingLibrary {
	private readonly library: CrispEmbedLibrary;
	private readonly logCallback: JSCallback;
	private context: Pointer | undefined;
	private diagnostic = "";

	constructor(libraryPath: string) {
		this.library = dlopen(libraryPath, symbols) as unknown as CrispEmbedLibrary;
		this.logCallback = new JSCallback(
			(message: Pointer | null) => {
				if (!message) return;
				this.diagnostic = new CString(message)
					.toString()
					.slice(0, MAX_DIAGNOSTIC_BYTES)
					.replace(/[\r\n]+$/u, "");
			},
			{ args: ["ptr", "ptr"], returns: "void" },
		);
		this.library.symbols.crispembed_set_log_callback(this.logCallback.ptr, null);
	}

	prepare(modelPath: string): void {
		this.dispose();
		this.diagnostic = "";
		process.env.CRISPEMBED_FORCE_CPU = "1";
		const modelPathBytes = cString(modelPath);
		const context = this.library.symbols.crispembed_init(ptr(modelPathBytes), 1);
		if (!context) throw this.nativeError("Could not initialize Bone's local GGUF embedding engine");
		this.context = context;
	}

	embed(mode: 0 | 1, texts: readonly string[]): Float32Array {
		const context = this.context;
		if (!context) throw new Error("Local semantic engine is not prepared");
		if (texts.length === 0 || texts.length > MAX_TEXT_COUNT)
			throw new Error("Embedding request has an invalid text count");

		const encoded = texts.map((text) => {
			const value = cString(text);
			if (value.byteLength - 1 > MAX_TEXT_BYTES)
				throw new Error("Embedding text exceeds the maximum supported size");
			return value;
		});
		const textPointers = new BigUint64Array(encoded.length);
		for (const [index, value] of encoded.entries()) textPointers[index] = BigInt(ptr(value));

		const prefix = cString(mode === 0 ? "query: Find the previous Bone conversation relevant to: " : "passage: ");
		this.library.symbols.crispembed_set_prefix(context, ptr(prefix));
		this.diagnostic = "";
		const dimensions = new Int32Array(1);
		const vectors = this.library.symbols.crispembed_encode_batch(
			context,
			ptr(textPointers),
			texts.length,
			ptr(dimensions),
		);
		if (!vectors || dimensions[0] !== DIMENSIONS)
			throw this.nativeError("Local GGUF embedding engine returned invalid vectors");
		const elementCount = texts.length * DIMENSIONS;
		return new Float32Array(toArrayBuffer(vectors, 0, elementCount * Float32Array.BYTES_PER_ELEMENT).slice(0));
	}

	dispose(): void {
		if (!this.context) return;
		this.library.symbols.crispembed_free(this.context);
		this.context = undefined;
	}

	close(): void {
		this.dispose();
		this.logCallback.close();
		this.library.close();
	}

	private nativeError(fallback: string): Error {
		return new Error(this.diagnostic ? `${fallback}: ${this.diagnostic}` : fallback);
	}
}
