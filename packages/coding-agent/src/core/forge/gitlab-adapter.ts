import { valid } from "semver";
import type {
	ForgeCapability,
	ForgePage,
	GitLabAdapterOptions,
	GitLabCapabilityId,
	GitLabJob,
	GitLabPipeline,
	GitLabProjectResource,
	GitLabResource,
	GitLabVersion,
	GitLabWikiPage,
	GitLabWriteBody,
} from "./contracts.ts";
import { ForgeError } from "./errors.ts";
import { ForgeHttpClient } from "./http-client.ts";

type Project = string | number;

interface VersionResponse {
	version?: unknown;
	revision?: unknown;
}

const CAPABILITY_PATHS: ReadonlyArray<{ id: GitLabCapabilityId; path: (project: string) => string }> = [
	{ id: "issue", path: (project) => `/api/v4/projects/${project}/issues?per_page=1` },
	{ id: "milestone", path: (project) => `/api/v4/projects/${project}/milestones?per_page=1` },
	{ id: "merge_request", path: (project) => `/api/v4/projects/${project}/merge_requests?per_page=1` },
	{ id: "wiki", path: (project) => `/api/v4/projects/${project}/wikis?per_page=1` },
	{ id: "pipeline", path: (project) => `/api/v4/projects/${project}/pipelines?per_page=1` },
	{ id: "job", path: (project) => `/api/v4/projects/${project}/jobs?per_page=1` },
	{ id: "release", path: (project) => `/api/v4/projects/${project}/releases?per_page=1` },
];

function encodedProject(project: Project): string {
	return encodeURIComponent(String(project));
}

function requireResource(value: unknown, operation: string): GitLabResource {
	if (typeof value !== "object" || value === null || typeof (value as { id?: unknown }).id !== "number") {
		throw new ForgeError("invalid_remote_response", `GitLab ${operation} response is missing a numeric id`, {
			operation,
		});
	}
	return value as GitLabResource;
}

function requireResourceList(value: unknown, operation: string): GitLabResource[] {
	if (!Array.isArray(value)) {
		throw new ForgeError("invalid_remote_response", `GitLab ${operation} response is not a list`, { operation });
	}
	return value.map((item) => requireResource(item, operation));
}

function requireWikiPage(value: unknown, operation: string): GitLabWikiPage {
	if (typeof value !== "object" || value === null || typeof (value as { slug?: unknown }).slug !== "string") {
		throw new ForgeError("invalid_remote_response", `GitLab ${operation} response is missing slug`, { operation });
	}
	return value as GitLabWikiPage;
}

function requireRelease(value: unknown, operation: string): Record<string, unknown> {
	if (typeof value !== "object" || value === null || typeof (value as { tag_name?: unknown }).tag_name !== "string") {
		throw new ForgeError("invalid_remote_response", `GitLab ${operation} response is missing tag_name`, {
			operation,
		});
	}
	return value as Record<string, unknown>;
}

function headerValue(value: string | string[] | undefined): string | undefined {
	return Array.isArray(value) ? value[0] : value;
}

export function parseGitLabVersion(value: string, revision?: string): GitLabVersion {
	const raw = value.trim();
	const parsed = valid(raw);
	if (!parsed) throw new ForgeError("invalid_remote_response", `GitLab returned an invalid version: ${raw}`);
	const suffix = raw.toLowerCase();
	return {
		raw,
		semver: parsed,
		revision,
		edition: /(?:^|[.-])ee(?:[.-]|$)/.test(suffix) ? "ee" : /(?:^|[.-])ce(?:[.-]|$)/.test(suffix) ? "ce" : undefined,
	};
}

export class GitLabAdapter {
	readonly client: ForgeHttpClient;

	constructor(options: GitLabAdapterOptions) {
		this.client = new ForgeHttpClient({
			baseUrl: options.baseUrl,
			provider: "gitlab",
			authHeaders: { "private-token": options.token },
			redactedSecrets: [options.token],
			allowedHosts: options.allowedHosts,
			dispatcher: options.dispatcher,
			allowInsecureHttp: options.allowInsecureHttp,
			requestTimeoutMs: options.requestTimeoutMs,
		});
	}

	async getVersion(signal?: AbortSignal): Promise<GitLabVersion> {
		const response = await this.client.request<VersionResponse>("GET", "/api/v4/version", { signal });
		if (typeof response.data?.version !== "string") {
			throw new ForgeError("invalid_remote_response", "GitLab version response is missing version");
		}
		return parseGitLabVersion(
			response.data.version,
			typeof response.data.revision === "string" ? response.data.revision : undefined,
		);
	}

	async getCapabilities(project: Project, signal?: AbortSignal): Promise<ForgeCapability[]> {
		const encoded = encodedProject(project);
		const baseCapabilities = await Promise.all(
			CAPABILITY_PATHS.map(async ({ id, path }): Promise<ForgeCapability> => {
				try {
					await this.client.request<unknown>("GET", path(encoded), { signal });
					return { id, state: "supported" };
				} catch (error) {
					if (!(error instanceof ForgeError)) return { id, state: "unknown", reason: "Probe failed" };
					if (error.code === "permission_denied" || error.code === "authentication_required") {
						return { id, state: "forbidden", reason: error.message };
					}
					if (error.code === "not_found") {
						return {
							id,
							state: id === "wiki" || id === "pipeline" ? "disabled" : "unsupported",
							reason: error.message,
						};
					}
					if (error.details.status === 405) return { id, state: "unsupported", reason: error.message };
					if (error.details.status === 410) return { id, state: "disabled", reason: error.message };
					return { id, state: "unknown", reason: error.message };
				}
			}),
		);

		const job = baseCapabilities.find((item) => item.id === "job");
		return [
			...baseCapabilities,
			{
				id: "merge_request.approvals",
				state: "unknown",
				reason: "Merge request approvals are checked lazily against a specific merge request",
			},
			{
				id: "pipeline.manual_job",
				state: job?.state === "forbidden" ? "forbidden" : "unknown",
				reason: "Manual jobs are checked lazily against a specific playable job",
			},
		];
	}

	private async list<T extends GitLabResource>(
		path: string,
		query: Record<string, string | number | boolean | undefined> = {},
		signal?: AbortSignal,
	): Promise<ForgePage<T>> {
		const response = await this.client.request<unknown>("GET", path, { query: { per_page: 10, ...query }, signal });
		const items = requireResourceList(response.data, path) as T[];
		const nextCursor = headerValue(response.headers["x-next-page"]);
		return { items, nextCursor: nextCursor || undefined, hasMore: Boolean(nextCursor) };
	}

	private async mutate<T extends GitLabResource>(
		method: "POST" | "PUT" | "DELETE",
		path: string,
		body?: GitLabWriteBody,
		signal?: AbortSignal,
	): Promise<T | undefined> {
		const response = await this.client.request<unknown>(method, path, { body, signal });
		if (method === "DELETE" && response.data === undefined) return undefined;
		return requireResource(response.data, path) as T;
	}

	private async mutateWiki(
		method: "POST" | "PUT",
		path: string,
		body: GitLabWriteBody,
		signal?: AbortSignal,
	): Promise<GitLabWikiPage> {
		const response = await this.client.request<unknown>(method, path, { body, signal });
		return requireWikiPage(response.data, path);
	}

	listIssues(
		project: Project,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	): Promise<ForgePage<GitLabProjectResource>> {
		return this.list(`/api/v4/projects/${encodedProject(project)}/issues`, query, signal);
	}

	createIssue(
		project: Project,
		body: GitLabWriteBody,
		signal?: AbortSignal,
	): Promise<GitLabProjectResource | undefined> {
		return this.mutate("POST", `/api/v4/projects/${encodedProject(project)}/issues`, body, signal);
	}

	updateIssue(
		project: Project,
		iid: number,
		body: GitLabWriteBody,
		signal?: AbortSignal,
	): Promise<GitLabProjectResource | undefined> {
		return this.mutate("PUT", `/api/v4/projects/${encodedProject(project)}/issues/${iid}`, body, signal);
	}

	listMilestones(
		project: Project,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	): Promise<ForgePage<GitLabProjectResource>> {
		return this.list(`/api/v4/projects/${encodedProject(project)}/milestones`, query, signal);
	}

	createMilestone(
		project: Project,
		body: GitLabWriteBody,
		signal?: AbortSignal,
	): Promise<GitLabProjectResource | undefined> {
		return this.mutate("POST", `/api/v4/projects/${encodedProject(project)}/milestones`, body, signal);
	}

	updateMilestone(
		project: Project,
		milestoneId: number,
		body: GitLabWriteBody,
		signal?: AbortSignal,
	): Promise<GitLabProjectResource | undefined> {
		return this.mutate("PUT", `/api/v4/projects/${encodedProject(project)}/milestones/${milestoneId}`, body, signal);
	}

	listMergeRequests(
		project: Project,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	): Promise<ForgePage<GitLabProjectResource>> {
		return this.list(`/api/v4/projects/${encodedProject(project)}/merge_requests`, query, signal);
	}

	createMergeRequest(
		project: Project,
		body: GitLabWriteBody,
		signal?: AbortSignal,
	): Promise<GitLabProjectResource | undefined> {
		return this.mutate("POST", `/api/v4/projects/${encodedProject(project)}/merge_requests`, body, signal);
	}

	updateMergeRequest(
		project: Project,
		iid: number,
		body: GitLabWriteBody,
		signal?: AbortSignal,
	): Promise<GitLabProjectResource | undefined> {
		return this.mutate("PUT", `/api/v4/projects/${encodedProject(project)}/merge_requests/${iid}`, body, signal);
	}

	mergeMergeRequest(
		project: Project,
		iid: number,
		body: GitLabWriteBody = {},
		signal?: AbortSignal,
	): Promise<GitLabProjectResource | undefined> {
		return this.mutate(
			"PUT",
			`/api/v4/projects/${encodedProject(project)}/merge_requests/${iid}/merge`,
			body,
			signal,
		);
	}

	async listWikiPages(
		project: Project,
		query: Record<string, string | number | boolean | undefined> = {},
		signal?: AbortSignal,
	): Promise<ForgePage<GitLabWikiPage>> {
		const path = `/api/v4/projects/${encodedProject(project)}/wikis`;
		const response = await this.client.request<unknown>("GET", path, { query: { per_page: 10, ...query }, signal });
		if (!Array.isArray(response.data)) {
			throw new ForgeError("invalid_remote_response", "GitLab Wiki response is not a list", { operation: path });
		}
		const items = response.data.map((item) => requireWikiPage(item, path));
		const nextCursor = headerValue(response.headers["x-next-page"]);
		return { items, nextCursor: nextCursor || undefined, hasMore: Boolean(nextCursor) };
	}

	async getWikiPage(project: Project, slug: string, signal?: AbortSignal): Promise<GitLabWikiPage> {
		const response = await this.client.request<unknown>(
			"GET",
			`/api/v4/projects/${encodedProject(project)}/wikis/${encodeURIComponent(slug)}`,
			{ signal },
		);
		return requireWikiPage(response.data, "get Wiki page");
	}

	createWikiPage(project: Project, body: GitLabWriteBody, signal?: AbortSignal): Promise<GitLabWikiPage> {
		return this.mutateWiki("POST", `/api/v4/projects/${encodedProject(project)}/wikis`, body, signal);
	}

	updateWikiPage(
		project: Project,
		slug: string,
		body: GitLabWriteBody,
		signal?: AbortSignal,
	): Promise<GitLabWikiPage> {
		return this.mutateWiki(
			"PUT",
			`/api/v4/projects/${encodedProject(project)}/wikis/${encodeURIComponent(slug)}`,
			body,
			signal,
		);
	}

	async deleteWikiPage(project: Project, slug: string, signal?: AbortSignal): Promise<void> {
		await this.client.request<unknown>(
			"DELETE",
			`/api/v4/projects/${encodedProject(project)}/wikis/${encodeURIComponent(slug)}`,
			{ signal },
		);
	}

	listPipelines(
		project: Project,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	): Promise<ForgePage<GitLabPipeline>> {
		return this.list(`/api/v4/projects/${encodedProject(project)}/pipelines`, query, signal);
	}

	createPipeline(project: Project, body: GitLabWriteBody, signal?: AbortSignal): Promise<GitLabPipeline | undefined> {
		return this.mutate("POST", `/api/v4/projects/${encodedProject(project)}/pipeline`, body, signal);
	}

	retryPipeline(project: Project, pipelineId: number, signal?: AbortSignal): Promise<GitLabPipeline | undefined> {
		return this.mutate(
			"POST",
			`/api/v4/projects/${encodedProject(project)}/pipelines/${pipelineId}/retry`,
			{},
			signal,
		);
	}

	cancelPipeline(project: Project, pipelineId: number, signal?: AbortSignal): Promise<GitLabPipeline | undefined> {
		return this.mutate(
			"POST",
			`/api/v4/projects/${encodedProject(project)}/pipelines/${pipelineId}/cancel`,
			{},
			signal,
		);
	}

	listPipelineJobs(
		project: Project,
		pipelineId: number,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	): Promise<ForgePage<GitLabJob>> {
		return this.list(`/api/v4/projects/${encodedProject(project)}/pipelines/${pipelineId}/jobs`, query, signal);
	}

	listReleases(
		project: Project,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	): Promise<ForgePage<Record<string, unknown>>> {
		const path = `/api/v4/projects/${encodedProject(project)}/releases`;
		return this.client
			.request<unknown>("GET", path, { query: { per_page: 10, ...query }, signal })
			.then((response) => {
				if (!Array.isArray(response.data)) {
					throw new ForgeError("invalid_remote_response", "GitLab release response is not a list", {
						operation: path,
					});
				}
				const nextCursor = headerValue(response.headers["x-next-page"]);
				return {
					items: response.data.map((item) => requireRelease(item, path)),
					nextCursor: nextCursor || undefined,
					hasMore: Boolean(nextCursor),
				};
			});
	}

	playJob(
		project: Project,
		jobId: number,
		body: GitLabWriteBody = {},
		signal?: AbortSignal,
	): Promise<GitLabJob | undefined> {
		return this.mutate("POST", `/api/v4/projects/${encodedProject(project)}/jobs/${jobId}/play`, body, signal);
	}

	retryJob(project: Project, jobId: number, signal?: AbortSignal): Promise<GitLabJob | undefined> {
		return this.mutate("POST", `/api/v4/projects/${encodedProject(project)}/jobs/${jobId}/retry`, {}, signal);
	}

	cancelJob(project: Project, jobId: number, signal?: AbortSignal): Promise<GitLabJob | undefined> {
		return this.mutate("POST", `/api/v4/projects/${encodedProject(project)}/jobs/${jobId}/cancel`, {}, signal);
	}
}
