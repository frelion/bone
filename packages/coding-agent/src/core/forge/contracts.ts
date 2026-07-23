export type ForgeProvider = "gitlab" | "github";

export type CapabilityState = "supported" | "unsupported" | "forbidden" | "disabled" | "unknown";

export type ForgeCapabilityId = string;

export type GitLabCapabilityId =
	| "issue"
	| "milestone"
	| "merge_request"
	| "wiki"
	| "pipeline"
	| "job"
	| "release"
	| "merge_request.approvals"
	| "pipeline.manual_job";

export interface ForgeRepositoryRef {
	provider: ForgeProvider;
	host: string;
	projectPath: string;
	remoteName: string;
	remoteUrl: string;
	rootDir: string;
}

export interface GitLabVersion {
	raw: string;
	semver: string;
	revision?: string;
	edition?: "ce" | "ee";
}

export interface ForgeCapability {
	id: ForgeCapabilityId;
	state: CapabilityState;
	reason?: string;
	minimumVersion?: string;
}

export interface ForgePage<T> {
	items: T[];
	nextCursor?: string;
	hasMore: boolean;
}

export interface GitLabResource {
	id: number;
	[key: string]: unknown;
}

export interface GitLabProjectResource extends GitLabResource {
	iid?: number;
	project_id?: number;
	state?: string;
	title?: string;
}

export interface GitLabPipeline extends GitLabResource {
	status?: string;
	ref?: string;
	sha?: string;
}

export interface GitLabJob extends GitLabResource {
	status?: string;
	name?: string;
	pipeline?: { id?: number; [key: string]: unknown };
}

export interface GitLabWikiPage {
	slug: string;
	title?: string;
	content?: string;
	[key: string]: unknown;
}

export type GitLabWriteBody = Record<string, unknown>;

export interface GitLabAdapterOptions {
	baseUrl: string;
	token: string;
	allowedHosts: readonly string[];
	dispatcher?: import("undici-client").Dispatcher;
	allowInsecureHttp?: boolean;
	requestTimeoutMs?: number;
}
