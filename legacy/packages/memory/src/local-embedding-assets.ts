import { join } from "node:path";

/**
 * The local semantic-search model is intentionally a Bone implementation
 * detail. Keeping its layout and manifest schema here prevents the normal
 * runtime and `bone setup` worker from silently accepting different assets.
 */
export const LOCAL_EMBEDDING_MODEL_ID = "cstr/multilingual-e5-small-GGUF";
export const LOCAL_EMBEDDING_CACHE_DIRECTORY = "bone-semantic-search-v2";
export const LOCAL_EMBEDDING_GGUF_FILE = "multilingual-e5-small-q8_0.gguf";
export const LOCAL_EMBEDDING_DIMENSIONS = 384;

export interface LocalEmbeddingAssetManifest {
	format: "bone-semantic-search-assets-v2";
	modelId: string;
	revision: string;
	files: Record<string, string>;
}

export function getLocalEmbeddingCacheDirectory(agentDir: string): string {
	return join(agentDir, "models", LOCAL_EMBEDDING_CACHE_DIRECTORY);
}

export function getLocalEmbeddingManifestPath(agentDir: string): string {
	return join(getLocalEmbeddingCacheDirectory(agentDir), "asset-manifest.json");
}

/** Asset paths are manifest-relative POSIX paths, never host paths. */
function isSafeAssetRelativePath(path: string): boolean {
	return (
		path.length > 0 &&
		!path.startsWith("/") &&
		!path.includes("\\") &&
		!path.split("/").some((segment) => segment === "..")
	);
}

/** Runtime validation for an untrusted on-disk asset manifest. */
export function isLocalEmbeddingAssetManifest(value: unknown): value is LocalEmbeddingAssetManifest {
	if (!value || typeof value !== "object") return false;
	const candidate = value as Partial<LocalEmbeddingAssetManifest>;
	return (
		candidate.format === "bone-semantic-search-assets-v2" &&
		candidate.modelId === LOCAL_EMBEDDING_MODEL_ID &&
		typeof candidate.revision === "string" &&
		/^[0-9a-f]{40}$/i.test(candidate.revision) &&
		candidate.files !== undefined &&
		candidate.files !== null &&
		typeof candidate.files === "object" &&
		Object.entries(candidate.files).every(
			([path, hash]) => isSafeAssetRelativePath(path) && typeof hash === "string" && /^[0-9a-f]{64}$/i.test(hash),
		) &&
		Object.keys(candidate.files).some((path) => path.endsWith(`/${LOCAL_EMBEDDING_GGUF_FILE}`))
	);
}
