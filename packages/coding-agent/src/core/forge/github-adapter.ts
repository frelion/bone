import type { Dispatcher } from "undici";
import type { ForgeCapability, ForgePage } from "./contracts.ts";
import { ForgeError } from "./errors.ts";
import { ForgeHttpClient } from "./http-client.ts";

export interface GitHubAdapterOptions {
	baseUrl: string;
	token: string;
	allowedHosts: readonly string[];
	dispatcher?: Dispatcher;
	allowInsecureHttp?: boolean;
	requestTimeoutMs?: number;
}

export interface GitHubResource {
	id: number;
	[key: string]: unknown;
}

export type GitHubWriteBody = Record<string, unknown>;

const GITHUB_API_VERSION = "2022-11-28";

function encodedRepository(repository: string): string {
	const segments = repository.split("/");
	if (segments.length !== 2 || segments.some((segment) => segment.length === 0)) {
		throw new ForgeError("validation_failed", `Invalid GitHub repository path: ${repository}`);
	}
	return segments.map(encodeURIComponent).join("/");
}

function requireResource(value: unknown, operation: string): GitHubResource {
	if (typeof value !== "object" || value === null || typeof (value as { id?: unknown }).id !== "number") {
		throw new ForgeError("invalid_remote_response", `GitHub ${operation} response is missing a numeric id`, {
			provider: "github",
			operation,
		});
	}
	return value as GitHubResource;
}

function requireResourceList(value: unknown, operation: string): GitHubResource[] {
	if (!Array.isArray(value)) {
		throw new ForgeError("invalid_remote_response", `GitHub ${operation} response is not a list`, {
			provider: "github",
			operation,
		});
	}
	return value.map((item) => requireResource(item, operation));
}

function headerValue(value: string | string[] | undefined): string | undefined {
	return Array.isArray(value) ? value[0] : value;
}

function nextPage(link: string | undefined): string | undefined {
	if (!link) return undefined;
	for (const part of link.split(",")) {
		if (!/;\s*rel="next"\s*$/.test(part)) continue;
		const match = /<([^>]+)>/.exec(part);
		if (!match) return undefined;
		try {
			return new URL(match[1]).searchParams.get("page") ?? undefined;
		} catch {
			return undefined;
		}
	}
	return undefined;
}

export class GitHubAdapter {
	readonly client: ForgeHttpClient;

	constructor(options: GitHubAdapterOptions) {
		this.client = new ForgeHttpClient({
			baseUrl: options.baseUrl,
			provider: "github",
			authHeaders: {
				authorization: `Bearer ${options.token}`,
				"x-github-api-version": GITHUB_API_VERSION,
				"user-agent": "bone-forge",
			},
			redactedSecrets: [options.token],
			allowedHosts: options.allowedHosts,
			dispatcher: options.dispatcher,
			allowInsecureHttp: options.allowInsecureHttp,
			requestTimeoutMs: options.requestTimeoutMs,
		});
	}

	async getCurrentUser(signal?: AbortSignal): Promise<GitHubResource> {
		const response = await this.client.request<unknown>("GET", "/user", { signal });
		return requireResource(response.data, "current user");
	}

	async getCapabilities(repository: string, signal?: AbortSignal): Promise<ForgeCapability[]> {
		const repo = encodedRepository(repository);
		const probes = [
			["issue", `/repos/${repo}/issues?per_page=1`],
			["milestone", `/repos/${repo}/milestones?per_page=1`],
			["pull_request", `/repos/${repo}/pulls?per_page=1`],
			["actions", `/repos/${repo}/actions/runs?per_page=1`],
			["release", `/repos/${repo}/releases?per_page=1`],
		] as const;
		const capabilities = await Promise.all(
			probes.map(async ([id, path]): Promise<ForgeCapability> => {
				try {
					await this.client.probe(path, { signal });
					return { id, state: "supported" };
				} catch (error) {
					if (!(error instanceof ForgeError)) return { id, state: "unknown", reason: "Probe failed" };
					if (error.code === "authentication_required" || error.code === "permission_denied") {
						return { id, state: "forbidden", reason: error.message };
					}
					if (error.code === "not_found") return { id, state: "disabled", reason: error.message };
					return { id, state: "unknown", reason: error.message };
				}
			}),
		);
		return [...capabilities, { id: "wiki", state: "unsupported", reason: "GitHub has no supported Wiki REST API" }];
	}

	private async list(
		path: string,
		query: Record<string, string | number | boolean | undefined> = {},
		signal?: AbortSignal,
		extract?: (value: unknown) => unknown,
	): Promise<ForgePage<GitHubResource>> {
		const response = await this.client.request<unknown>("GET", path, { query: { per_page: 10, ...query }, signal });
		const items = requireResourceList(extract ? extract(response.data) : response.data, path);
		const cursor = nextPage(headerValue(response.headers.link));
		return { items, nextCursor: cursor, hasMore: cursor !== undefined };
	}

	private async mutate(
		method: "POST" | "PATCH" | "PUT" | "DELETE",
		path: string,
		body?: GitHubWriteBody,
		signal?: AbortSignal,
	): Promise<GitHubResource | undefined> {
		const response = await this.client.request<unknown>(method, path, { body, signal });
		if (response.data === undefined) return undefined;
		return requireResource(response.data, path);
	}

	private async mutateResource(
		method: "POST" | "PATCH",
		path: string,
		body: GitHubWriteBody,
		signal?: AbortSignal,
	): Promise<GitHubResource> {
		const response = await this.client.request<unknown>(method, path, { body, signal });
		return requireResource(response.data, path);
	}

	listIssues(repository: string, query?: Record<string, string | number | boolean | undefined>, signal?: AbortSignal) {
		return this.list(`/repos/${encodedRepository(repository)}/issues`, query, signal);
	}

	searchIssues(
		repository: string,
		search: string,
		kind: "issue" | "pr",
		query: Record<string, string | number | boolean | undefined> = {},
		signal?: AbortSignal,
	) {
		encodedRepository(repository);
		const { state, ...pagination } = query;
		const normalizedState = state === "opened" ? "open" : state;
		const stateQualifier = normalizedState === "open" || normalizedState === "closed" ? ` is:${normalizedState}` : "";
		return this.list(
			"/search/issues",
			{ ...pagination, q: `${search} repo:${repository} is:${kind}${stateQualifier}` },
			signal,
			(value) => (typeof value === "object" && value !== null ? (value as { items?: unknown }).items : undefined),
		);
	}

	createIssue(repository: string, body: GitHubWriteBody, signal?: AbortSignal) {
		return this.mutate("POST", `/repos/${encodedRepository(repository)}/issues`, body, signal);
	}

	updateIssue(repository: string, number: number, body: GitHubWriteBody, signal?: AbortSignal) {
		return this.mutate("PATCH", `/repos/${encodedRepository(repository)}/issues/${number}`, body, signal);
	}

	listMilestones(
		repository: string,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	) {
		return this.list(`/repos/${encodedRepository(repository)}/milestones`, query, signal);
	}

	createMilestone(repository: string, body: GitHubWriteBody, signal?: AbortSignal) {
		return this.mutate("POST", `/repos/${encodedRepository(repository)}/milestones`, body, signal);
	}

	updateMilestone(repository: string, number: number, body: GitHubWriteBody, signal?: AbortSignal) {
		return this.mutate("PATCH", `/repos/${encodedRepository(repository)}/milestones/${number}`, body, signal);
	}

	listPullRequests(
		repository: string,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	) {
		return this.list(`/repos/${encodedRepository(repository)}/pulls`, query, signal);
	}

	createPullRequest(repository: string, body: GitHubWriteBody, signal?: AbortSignal) {
		return this.mutate("POST", `/repos/${encodedRepository(repository)}/pulls`, body, signal);
	}

	updatePullRequest(repository: string, number: number, body: GitHubWriteBody, signal?: AbortSignal) {
		return this.mutate("PATCH", `/repos/${encodedRepository(repository)}/pulls/${number}`, body, signal);
	}

	async mergePullRequest(
		repository: string,
		number: number,
		body: GitHubWriteBody = {},
		signal?: AbortSignal,
	): Promise<Record<string, unknown>> {
		const response = await this.client.request<unknown>(
			"PUT",
			`/repos/${encodedRepository(repository)}/pulls/${number}/merge`,
			{ body, signal },
		);
		if (
			typeof response.data !== "object" ||
			response.data === null ||
			typeof (response.data as { merged?: unknown }).merged !== "boolean"
		) {
			throw new ForgeError("invalid_remote_response", "GitHub merge response is missing merged status", {
				provider: "github",
				operation: "merge pull request",
			});
		}
		return response.data as Record<string, unknown>;
	}

	listWorkflowRuns(
		repository: string,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	) {
		return this.list(`/repos/${encodedRepository(repository)}/actions/runs`, query, signal, (value) =>
			typeof value === "object" && value !== null ? (value as { workflow_runs?: unknown }).workflow_runs : undefined,
		);
	}

	listWorkflowJobs(
		repository: string,
		runId: number,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	) {
		return this.list(`/repos/${encodedRepository(repository)}/actions/runs/${runId}/jobs`, query, signal, (value) =>
			typeof value === "object" && value !== null ? (value as { jobs?: unknown }).jobs : undefined,
		);
	}

	async rerunWorkflow(repository: string, runId: number, signal?: AbortSignal): Promise<void> {
		await this.client.request("POST", `/repos/${encodedRepository(repository)}/actions/runs/${runId}/rerun`, {
			body: {},
			signal,
		});
	}

	async cancelWorkflow(repository: string, runId: number, signal?: AbortSignal): Promise<void> {
		await this.client.request("POST", `/repos/${encodedRepository(repository)}/actions/runs/${runId}/cancel`, {
			body: {},
			signal,
		});
	}

	listReleases(
		repository: string,
		query?: Record<string, string | number | boolean | undefined>,
		signal?: AbortSignal,
	) {
		return this.list(`/repos/${encodedRepository(repository)}/releases`, query, signal);
	}

	createRelease(repository: string, body: GitHubWriteBody, signal?: AbortSignal) {
		return this.mutateResource("POST", `/repos/${encodedRepository(repository)}/releases`, body, signal);
	}

	updateRelease(repository: string, releaseId: number, body: GitHubWriteBody, signal?: AbortSignal) {
		return this.mutateResource(
			"PATCH",
			`/repos/${encodedRepository(repository)}/releases/${releaseId}`,
			body,
			signal,
		);
	}
}
