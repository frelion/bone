/** Local resource metadata used by Bone's resource loader and settings UI. */
export interface PathMetadata {
	source: string;
	scope: "user" | "project" | "temporary";
	origin: "top-level";
	baseDir?: string;
}

export interface ResolvedResource {
	path: string;
	enabled: boolean;
	metadata: PathMetadata;
}

export interface ResolvedPaths {
	skills: ResolvedResource[];
	prompts: ResolvedResource[];
	themes: ResolvedResource[];
}
