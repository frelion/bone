import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { promisify } from "node:util";
import type { Dispatcher } from "undici";
import { getAgentDir } from "../../config.ts";
import { enforceForgeApproval, type ForgeOperationRisk } from "./approval.ts";
import { type ForgeInstanceConfig, findForgeInstance, loadForgeConfig, resolveForgeProvider } from "./config.ts";
import { parseGitRemote, resolveForgeContext } from "./context-resolver.ts";
import type { ForgeRepositoryRef, GitLabWriteBody } from "./contracts.ts";
import { ForgeCredentialStore } from "./credential-store.ts";
import { ForgeError } from "./errors.ts";
import { GitHubAdapter, type GitHubWriteBody } from "./github-adapter.ts";
import { GitLabAdapter } from "./gitlab-adapter.ts";
import { ForgeMutationJournal } from "./mutation-journal.ts";
import { assertPublicNetworkHostname, createPublicNetworkDispatcher } from "./network-security.ts";
import {
	enforceForgePolicy,
	evaluateForgePolicy,
	type ForgePolicy,
	type ForgePolicyFacts,
	type ForgePolicyStage,
	loadForgePolicy,
} from "./policy.ts";
import type { ForgeService, ForgeToolContext, ForgeToolName } from "./tools.ts";

const execFileAsync = promisify(execFile);

interface ResolvedForge {
	repository: ForgeRepositoryRef;
	instance: ForgeInstanceConfig;
	adapter: GitLabAdapter | GitHubAdapter;
	credentialIdentity: string;
}

const MAX_POLICY_FACT_PAGES = 20;

function defaultInstance(repository: ForgeRepositoryRef): ForgeInstanceConfig | undefined {
	if (repository.host === "gitlab.com") {
		return {
			provider: "gitlab",
			host: "gitlab.com",
			apiBaseUrl: "https://gitlab.com",
			credential: "gitlab:gitlab.com",
			allowPrivateNetwork: false,
		};
	}
	if (repository.host === "github.com") {
		return {
			provider: "github",
			host: "github.com",
			apiBaseUrl: "https://api.github.com",
			credential: "github:github.com",
			allowPrivateNetwork: false,
		};
	}
	return undefined;
}

function environmentToken(provider: "gitlab" | "github"): string | undefined {
	return provider === "gitlab"
		? (process.env.GITLAB_TOKEN ?? process.env.GITLAB_PRIVATE_TOKEN)
		: (process.env.GITHUB_TOKEN ?? process.env.GH_TOKEN);
}

function numberInput(value: unknown, name: string): number {
	const parsed = typeof value === "number" ? value : typeof value === "string" ? Number(value) : Number.NaN;
	if (!Number.isSafeInteger(parsed) || parsed <= 0) {
		throw new ForgeError("validation_failed", `${name} must be a positive integer`);
	}
	return parsed;
}

function stringInput(value: unknown, name: string): string {
	if (typeof value !== "string" || value.trim().length === 0) {
		throw new ForgeError("validation_failed", `${name} must be a non-empty string`);
	}
	return value;
}

function inputBody(input: Record<string, unknown>): Record<string, unknown> {
	const body = input.input;
	if (body === undefined) return {};
	if (typeof body !== "object" || body === null || Array.isArray(body)) {
		throw new ForgeError("validation_failed", "input must be an object");
	}
	return body as Record<string, unknown>;
}

function gitLabBody(body: Record<string, unknown>): GitLabWriteBody {
	for (const [key, value] of Object.entries(body)) {
		const valid =
			value === null ||
			typeof value === "string" ||
			typeof value === "number" ||
			typeof value === "boolean" ||
			(Array.isArray(value) && value.every((item) => typeof item === "string"));
		if (!valid) throw new ForgeError("validation_failed", `GitLab field ${key} has an unsupported value`);
	}
	return body as GitLabWriteBody;
}

function canonical(value: unknown): unknown {
	if (Array.isArray(value)) return value.map(canonical);
	if (typeof value !== "object" || value === null) return value;
	return Object.fromEntries(
		Object.entries(value as Record<string, unknown>)
			.sort(([left], [right]) => left.localeCompare(right))
			.map(([key, entry]) => [key, canonical(entry)]),
	);
}

function fingerprint(toolName: ForgeToolName, repository: ForgeRepositoryRef, input: Record<string, unknown>): string {
	return createHash("sha256")
		.update(JSON.stringify(canonical({ toolName, host: repository.host, project: repository.projectPath, input })))
		.digest("hex");
}

function operationRisk(toolName: ForgeToolName, action: string): ForgeOperationRisk {
	if (
		toolName === "forge_context" ||
		toolName === "forge_query" ||
		toolName === "forge_audit" ||
		toolName === "forge_watch"
	) {
		return "read";
	}
	if (toolName === "forge_transition" && (action === "prepare_merge" || action === "prepare_release")) return "read";
	if (["merge", "delete", "cancel", "cancel_job"].includes(action)) return "destructive";
	if (
		(toolName === "forge_release" && action === "create") ||
		(toolName === "forge_transition" && action === "release") ||
		["close", "reopen", "approve", "retry", "retry_job", "play_job", "trigger"].includes(action)
	) {
		return "sensitive";
	}
	return "routine";
}

function linkedIssue(text: string): boolean {
	return /(?:^|[\s([])(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)?\s*(?:[\w.-]+\/[\w.-]+)?#\d+\b/im.test(text);
}

function encodedGitHubProject(project: string): string {
	return project.split("/").map(encodeURIComponent).join("/");
}

function objectValue(value: unknown, operation: string): Record<string, unknown> {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		throw new ForgeError("invalid_remote_response", `${operation} response is not an object`, { operation });
	}
	return value as Record<string, unknown>;
}

function arrayValue(value: unknown, operation: string): unknown[] {
	if (!Array.isArray(value)) {
		throw new ForgeError("invalid_remote_response", `${operation} response is not a list`, { operation });
	}
	return value;
}

function headerValue(value: string | string[] | undefined): string | undefined {
	return Array.isArray(value) ? value[0] : value;
}

function githubHasNextPage(link: string | undefined): boolean {
	return typeof link === "string" && link.split(",").some((part) => /;\s*rel="next"\s*$/.test(part));
}

async function localFacts(cwd: string): Promise<ForgePolicyFacts> {
	const [{ stdout: branch }, { stdout: status }] = await Promise.all([
		execFileAsync("git", ["--no-optional-locks", "branch", "--show-current"], { cwd, encoding: "utf8" }),
		execFileAsync("git", ["--no-optional-locks", "status", "--porcelain"], { cwd, encoding: "utf8" }),
	]);
	return { branch: branch.trim(), worktreeClean: status.trim().length === 0 };
}

async function delay(ms: number, signal?: AbortSignal): Promise<void> {
	if (signal?.aborted) throw new ForgeError("remote_failure", "Forge watch was cancelled");
	await new Promise<void>((resolve, reject) => {
		const cleanup = () => signal?.removeEventListener("abort", abort);
		const timeout = setTimeout(() => {
			cleanup();
			resolve();
		}, ms);
		const abort = () => {
			clearTimeout(timeout);
			cleanup();
			reject(new ForgeError("remote_failure", "Forge watch was cancelled"));
		};
		signal?.addEventListener("abort", abort, { once: true });
	});
}

export interface CreateForgeServiceOptions {
	cwd: string;
	agentDir?: string;
	dispatcher?: Dispatcher;
}

class DefaultForgeService implements ForgeService {
	private readonly cwd: string;
	private readonly agentDir: string;
	private readonly credentials: ForgeCredentialStore;
	private readonly journal: ForgeMutationJournal;
	private readonly dispatcher?: Dispatcher;
	private readonly publicDispatcher: Dispatcher;
	private readonly capabilities = new Map<string, number>();

	constructor(options: CreateForgeServiceOptions) {
		this.cwd = options.cwd;
		this.agentDir = options.agentDir ?? getAgentDir();
		this.credentials = new ForgeCredentialStore(this.agentDir);
		this.journal = new ForgeMutationJournal(this.agentDir);
		this.dispatcher = options.dispatcher;
		this.publicDispatcher = options.dispatcher ?? createPublicNetworkDispatcher();
	}

	private async resolve(input: Record<string, unknown>): Promise<ResolvedForge> {
		const remote = typeof input.remote === "string" ? input.remote : "origin";
		const repository =
			/^(?:https?|ssh|git):\/\//.test(remote) || remote.startsWith("git@")
				? parseGitRemote(remote, "explicit", this.cwd)
				: await resolveForgeContext(this.cwd, remote);
		if (typeof input.project === "string" && input.project.trim()) repository.projectPath = input.project.trim();

		const config = loadForgeConfig(this.agentDir);
		repository.provider = resolveForgeProvider(config, repository.host, repository.provider);
		const instance = findForgeInstance(config, repository.provider, repository.host) ?? defaultInstance(repository);
		if (!instance) {
			throw new ForgeError(
				"unsafe_remote",
				`Forge instance ${repository.provider}:${repository.host} must be explicitly configured in forge.json`,
			);
		}
		const credentialKey = instance.credential ?? `${instance.provider}:${instance.host}`;
		const token = this.credentials.resolve(credentialKey)?.token ?? environmentToken(instance.provider);
		if (!token) {
			throw new ForgeError("authentication_required", `No Forge credential is configured for ${credentialKey}`);
		}
		const allowedHosts = [new URL(instance.apiBaseUrl).host];
		if (!instance.allowPrivateNetwork) assertPublicNetworkHostname(new URL(instance.apiBaseUrl).hostname);
		const dispatcher = instance.allowPrivateNetwork ? this.dispatcher : this.publicDispatcher;
		const common = { baseUrl: instance.apiBaseUrl, token, allowedHosts, dispatcher };
		const adapter = instance.provider === "gitlab" ? new GitLabAdapter(common) : new GitHubAdapter(common);
		return {
			repository,
			instance,
			adapter,
			credentialIdentity: createHash("sha256").update(token).digest("hex"),
		};
	}

	async execute(
		toolName: ForgeToolName,
		input: Record<string, unknown>,
		signal: AbortSignal | undefined,
		context: ForgeToolContext,
	): Promise<unknown> {
		const resolved = await this.resolve(input);
		if (toolName === "forge_context") {
			if (input.refresh === true) this.capabilities.clear();
			return this.context(resolved, signal);
		}
		if (toolName === "forge_query") return this.query(resolved, input, signal, true);
		if (toolName === "forge_audit") return this.audit(resolved, input, context, signal);
		if (toolName === "forge_watch") return this.watch(resolved, input, signal);

		const action =
			toolName === "forge_transition"
				? stringInput(input.transition, "transition")
				: stringInput(input.action, "action");
		const risk = operationRisk(toolName, action);
		await this.ensureCapability(resolved, toolName, action, input, signal);
		const policy = loadForgePolicy(context.cwd, context.projectTrusted);
		if (policy?.provider && policy.provider !== resolved.repository.provider) {
			throw new ForgeError("policy_denied", `Forge policy requires provider ${policy.provider}`, {
				provider: resolved.repository.provider,
				operation: action,
				expectedProvider: policy.provider,
			});
		}
		let confirmed = false;
		const initialApproval = { interactive: context.interactive, confirmed };
		try {
			enforceForgeApproval(risk, policy, initialApproval);
		} catch (error) {
			if (!(error instanceof Error) || !("requiresConfirmation" in error) || error.requiresConfirmation !== true)
				throw error;
			confirmed = await context.confirm(
				`Confirm ${toolName}`,
				`${action} ${resolved.repository.provider}:${resolved.repository.projectPath} (${risk})`,
			);
			enforceForgeApproval(risk, policy, { interactive: context.interactive, confirmed });
		}

		if (toolName === "forge_transition" && (action === "prepare_merge" || action === "prepare_release")) {
			return this.transition(resolved, action, input, signal, context);
		}
		const requestId = stringInput(input.requestId, "requestId");
		const operationFingerprint = fingerprint(toolName, resolved.repository, input);
		const begin = this.journal.begin(requestId, operationFingerprint);
		if (begin.action === "replay") return begin.result;
		if (begin.action === "in_progress" || begin.action === "ambiguous") {
			throw new ForgeError("ambiguous_result", `Forge mutation ${requestId} is ${begin.action}`, {
				operation: action,
			});
		}
		try {
			const result =
				toolName === "forge_transition"
					? await this.transition(resolved, action, input, signal, context)
					: await this.mutate(resolved, toolName, action, input, signal, context);
			this.journal.complete(requestId, operationFingerprint, result);
			return result;
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			if (
				error instanceof ForgeError &&
				(error.code === "remote_failure" || error.code === "invalid_remote_response")
			) {
				this.journal.markAmbiguous(requestId, operationFingerprint, message);
			} else {
				this.journal.fail(requestId, operationFingerprint, message);
			}
			throw error;
		}
	}

	private async context(resolved: ResolvedForge, signal?: AbortSignal): Promise<unknown> {
		if (resolved.adapter instanceof GitLabAdapter) {
			const [version, capabilities] = await Promise.all([
				resolved.adapter.getVersion(signal),
				resolved.adapter.getCapabilities(resolved.repository.projectPath, signal),
			]);
			return { repository: resolved.repository, instance: resolved.instance, version, capabilities };
		}
		const [user, capabilities] = await Promise.all([
			resolved.adapter.getCurrentUser(signal),
			resolved.adapter.getCapabilities(resolved.repository.projectPath, signal),
		]);
		return { repository: resolved.repository, instance: resolved.instance, user, capabilities };
	}

	private async query(
		resolved: ResolvedForge,
		input: Record<string, unknown>,
		signal?: AbortSignal,
		checkCapability = true,
	): Promise<unknown> {
		const resource = stringInput(input.resource, "resource");
		if (checkCapability) await this.ensureResourceCapability(resolved, resource, signal);
		const query = {
			state: typeof input.state === "string" ? input.state : undefined,
			search: typeof input.search === "string" ? input.search : undefined,
			page: typeof input.cursor === "string" ? input.cursor : undefined,
			per_page: typeof input.limit === "number" ? input.limit : undefined,
		};
		const project = resolved.repository.projectPath;
		if (resolved.adapter instanceof GitLabAdapter) {
			switch (resource) {
				case "issue":
					return resolved.adapter.listIssues(project, query, signal);
				case "milestone":
					return resolved.adapter.listMilestones(project, query, signal);
				case "change":
					return resolved.adapter.listMergeRequests(project, query, signal);
				case "wiki":
					return resolved.adapter.listWikiPages(project, signal);
				case "pipeline":
					return resolved.adapter.listPipelines(project, query, signal);
				case "job":
					return resolved.adapter.listPipelineJobs(project, numberInput(input.id, "id"), signal);
				case "release":
					return resolved.adapter.client
						.request("GET", `/api/v4/projects/${encodeURIComponent(project)}/releases`, { query, signal })
						.then((response) => response.data);
			}
		}
		if (!(resolved.adapter instanceof GitHubAdapter)) {
			throw new ForgeError("remote_failure", "Forge provider adapter does not match the resolved repository");
		}
		if (resource === "wiki") {
			throw new ForgeError("unsupported_capability", "GitHub Wiki does not have a supported REST API", {
				capability: "wiki",
			});
		}
		switch (resource) {
			case "issue":
				return resolved.adapter.listIssues(project, query, signal);
			case "milestone":
				return resolved.adapter.listMilestones(project, query, signal);
			case "change":
				return resolved.adapter.listPullRequests(project, query, signal);
			case "pipeline":
				return resolved.adapter.listWorkflowRuns(project, query, signal);
			case "release":
				return resolved.adapter.listReleases(project, query, signal);
			default:
				throw new ForgeError("unsupported_capability", `GitHub query does not support ${resource}`, {
					capability: resource,
				});
		}
	}

	private async audit(
		resolved: ResolvedForge,
		input: Record<string, unknown>,
		context: ForgeToolContext,
		signal?: AbortSignal,
	): Promise<unknown> {
		const policy = loadForgePolicy(context.cwd, context.projectTrusted);
		if (!policy) return { compliant: true, policy: "not_configured", violations: [] };
		if (policy.provider && policy.provider !== resolved.repository.provider) {
			return {
				compliant: false,
				violations: [{ code: "provider_mismatch", message: `Policy requires ${policy.provider}` }],
			};
		}
		const workflow = typeof input.workflow === "string" ? input.workflow : "current";
		const stage: ForgePolicyStage = workflow === "current" ? "submit_review" : (workflow as ForgePolicyStage);
		if (stage === "merge") await this.ensureResourceCapability(resolved, "change", signal);
		if (stage === "release") await this.ensureResourceCapability(resolved, "release", signal);
		const facts =
			stage === "merge"
				? await this.remoteMergeFacts(resolved, numberInput(input.changeId, "changeId"), policy, signal)
				: stage === "release"
					? await this.remoteReleaseFacts(resolved, inputBody(input), policy, signal)
					: await localFacts(context.cwd);
		const violations = evaluateForgePolicy(policy, facts, stage);
		return { compliant: violations.length === 0, stage, facts, violations };
	}

	private async watch(
		resolved: ResolvedForge,
		input: Record<string, unknown>,
		signal?: AbortSignal,
	): Promise<unknown> {
		const until = Array.isArray(input.until)
			? input.until.filter((value): value is string => typeof value === "string")
			: [];
		const timeoutMs = (typeof input.timeoutSeconds === "number" ? input.timeoutSeconds : 600) * 1_000;
		const intervalMs = (typeof input.pollIntervalSeconds === "number" ? input.pollIntervalSeconds : 5) * 1_000;
		const deadline = Date.now() + timeoutMs;
		const resource = stringInput(input.resource, "resource");
		if (resolved.adapter instanceof GitLabAdapter && resource === "review") {
			const project = encodeURIComponent(resolved.repository.projectPath);
			const changeId = numberInput(input.id, "id");
			await this.ensurePathCapability(
				resolved,
				"merge_request.approvals",
				`/api/v4/projects/${project}/merge_requests/${changeId}/approvals`,
				signal,
			);
		} else {
			await this.ensureResourceCapability(resolved, resource, signal);
		}
		while (true) {
			const result = await this.readWatchState(resolved, resource, input.id, signal);
			if (until.some((value) => value.toLowerCase() === result.state.toLowerCase())) return result;
			if (Date.now() >= deadline)
				throw new ForgeError("remote_failure", `Forge watch timed out after ${timeoutMs / 1_000}s`);
			await delay(Math.min(intervalMs, Math.max(0, deadline - Date.now())), signal);
		}
	}

	private async readWatchState(
		resolved: ResolvedForge,
		resource: string,
		id: unknown,
		signal?: AbortSignal,
	): Promise<{ state: string; data: unknown }> {
		const numericId = numberInput(id, "id");
		if (resolved.adapter instanceof GitLabAdapter) {
			const project = encodeURIComponent(resolved.repository.projectPath);
			const path =
				resource === "pipeline"
					? `/api/v4/projects/${project}/pipelines/${numericId}`
					: resource === "job"
						? `/api/v4/projects/${project}/jobs/${numericId}`
						: resource === "change"
							? `/api/v4/projects/${project}/merge_requests/${numericId}`
							: resource === "review"
								? `/api/v4/projects/${project}/merge_requests/${numericId}/approvals`
								: undefined;
			if (!path) throw new ForgeError("unsupported_capability", `Cannot watch GitLab ${resource}`);
			const response = await resolved.adapter.client.request<unknown>("GET", path, { signal });
			const data = objectValue(response.data, `GitLab ${resource}`);
			if (resource === "review" && typeof data.approvals_left !== "number") {
				throw new ForgeError("invalid_remote_response", "GitLab approval response is missing approvals_left");
			}
			const state =
				resource === "review"
					? data.approvals_left === 0
						? "approved"
						: "pending"
					: typeof data.status === "string"
						? data.status
						: typeof data.state === "string"
							? data.state
							: "unknown";
			return { state, data };
		}
		const project = encodedGitHubProject(resolved.repository.projectPath);
		const path =
			resource === "pipeline"
				? `/repos/${project}/actions/runs/${numericId}`
				: resource === "job"
					? `/repos/${project}/actions/jobs/${numericId}`
					: resource === "change"
						? `/repos/${project}/pulls/${numericId}`
						: resource === "review"
							? `/repos/${project}/pulls/${numericId}/reviews`
							: undefined;
		if (!path) throw new ForgeError("unsupported_capability", `Cannot watch GitHub ${resource}`);
		if (resource === "review") {
			const reviews = await this.fetchPolicyArrayPages(resolved, path, "GitHub pull request reviews", signal);
			const currentStates = new Map<string, string>();
			for (const review of reviews) {
				const value = objectValue(review, "GitHub pull request review");
				const user =
					typeof value.user === "object" && value.user !== null
						? (value.user as { login?: unknown }).login
						: undefined;
				if (typeof user === "string" && typeof value.state === "string") {
					currentStates.set(user, value.state.toUpperCase());
				}
			}
			const approved = [...currentStates.values()].some((state) => state === "APPROVED");
			return { state: approved ? "approved" : "pending", data: reviews };
		}
		const response = await resolved.adapter.client.request<unknown>("GET", path, { signal });
		const data = objectValue(response.data, `GitHub ${resource}`);
		const state =
			typeof data.conclusion === "string"
				? data.conclusion
				: typeof data.status === "string"
					? data.status
					: typeof data.state === "string"
						? data.state
						: "unknown";
		return { state, data };
	}

	private async ensureResourceCapability(
		resolved: ResolvedForge,
		resource: string,
		signal?: AbortSignal,
	): Promise<void> {
		const capability =
			resolved.adapter instanceof GitLabAdapter
				? resource === "approval"
					? "merge_request.approvals"
					: resource === "change" || resource === "review"
						? "merge_request"
						: resource
				: resource === "change" || resource === "review"
					? "pull_request"
					: resource === "pipeline" || resource === "job"
						? "actions"
						: resource;
		const project =
			resolved.adapter instanceof GitLabAdapter
				? encodeURIComponent(resolved.repository.projectPath)
				: encodedGitHubProject(resolved.repository.projectPath);
		const path =
			resolved.adapter instanceof GitLabAdapter
				? capability === "issue"
					? `/api/v4/projects/${project}/issues?per_page=1`
					: capability === "milestone"
						? `/api/v4/projects/${project}/milestones?per_page=1`
						: capability === "merge_request"
							? `/api/v4/projects/${project}/merge_requests?per_page=1`
							: capability === "merge_request.approvals"
								? `/api/v4/projects/${project}/approvals`
								: capability === "wiki"
									? `/api/v4/projects/${project}/wikis?per_page=1`
									: capability === "pipeline"
										? `/api/v4/projects/${project}/pipelines?per_page=1`
										: capability === "job"
											? `/api/v4/projects/${project}/jobs?per_page=1`
											: capability === "release"
												? `/api/v4/projects/${project}/releases?per_page=1`
												: undefined
				: capability === "issue"
					? `/repos/${project}/issues?per_page=1`
					: capability === "milestone"
						? `/repos/${project}/milestones?per_page=1`
						: capability === "pull_request"
							? `/repos/${project}/pulls?per_page=1`
							: capability === "actions"
								? `/repos/${project}/actions/runs?per_page=1`
								: capability === "release"
									? `/repos/${project}/releases?per_page=1`
									: undefined;
		if (!path) {
			throw new ForgeError("unsupported_capability", `Forge capability ${capability} is unavailable`, {
				provider: resolved.repository.provider,
				capability,
			});
		}
		await this.ensurePathCapability(resolved, capability, path, signal);
	}

	private async ensurePathCapability(
		resolved: ResolvedForge,
		capability: string,
		path: string,
		signal?: AbortSignal,
	): Promise<void> {
		const key = `${resolved.repository.provider}:${resolved.repository.host}:${resolved.repository.projectPath}:${resolved.credentialIdentity}:${capability}`;
		if ((this.capabilities.get(key) ?? 0) > Date.now()) return;
		try {
			await resolved.adapter.client.request("GET", path, { signal });
			this.capabilities.set(key, Date.now() + 15 * 60 * 1_000);
		} catch (error) {
			if (
				error instanceof ForgeError &&
				(error.code === "not_found" || error.details.status === 405 || error.details.status === 410)
			) {
				throw new ForgeError("unsupported_capability", error.message, {
					...error.details,
					provider: resolved.repository.provider,
					capability,
				});
			}
			throw error;
		}
	}

	private async fetchPolicyArrayPages(
		resolved: ResolvedForge,
		path: string,
		operation: string,
		signal?: AbortSignal,
	): Promise<unknown[]> {
		const items: unknown[] = [];
		for (let page = 1; page <= MAX_POLICY_FACT_PAGES; page++) {
			const response = await resolved.adapter.client.request<unknown>("GET", path, {
				query: { per_page: 100, page },
				signal,
			});
			const pageItems = arrayValue(response.data, operation);
			items.push(...pageItems);
			const hasNext =
				resolved.adapter instanceof GitLabAdapter
					? Boolean(headerValue(response.headers["x-next-page"])) || pageItems.length === 100
					: githubHasNextPage(headerValue(response.headers.link)) || pageItems.length === 100;
			if (!hasNext) return items;
		}
		throw new ForgeError("policy_denied", `${operation} exceeds the ${MAX_POLICY_FACT_PAGES}-page audit limit`);
	}

	private async fetchGitHubCheckRunPages(
		resolved: ResolvedForge,
		path: string,
		signal?: AbortSignal,
	): Promise<unknown[]> {
		const items: unknown[] = [];
		for (let page = 1; page <= MAX_POLICY_FACT_PAGES; page++) {
			const response = await resolved.adapter.client.request<unknown>("GET", path, {
				query: { per_page: 100, page },
				signal,
			});
			const body = objectValue(response.data, "GitHub check runs");
			const pageItems = arrayValue(body.check_runs, "GitHub check runs");
			items.push(...pageItems);
			const total = typeof body.total_count === "number" ? body.total_count : undefined;
			const hasNext =
				githubHasNextPage(headerValue(response.headers.link)) ||
				pageItems.length === 100 ||
				(total !== undefined && items.length < total);
			if (!hasNext) return items;
		}
		throw new ForgeError("policy_denied", `GitHub check runs exceed the ${MAX_POLICY_FACT_PAGES}-page audit limit`);
	}

	private async ensureCapability(
		resolved: ResolvedForge,
		toolName: ForgeToolName,
		action: string,
		input: Record<string, unknown>,
		signal?: AbortSignal,
	): Promise<void> {
		if (resolved.adapter instanceof GitLabAdapter && toolName === "forge_change" && action === "approve") {
			const project = encodeURIComponent(resolved.repository.projectPath);
			const changeId = numberInput(input.id, "id");
			await this.ensurePathCapability(
				resolved,
				"merge_request.approvals",
				`/api/v4/projects/${project}/merge_requests/${changeId}/approvals`,
				signal,
			);
			return;
		}
		if (resolved.adapter instanceof GitLabAdapter && toolName === "forge_pipeline" && action === "play_job") {
			await this.ensureResourceCapability(resolved, "job", signal);
			const project = encodeURIComponent(resolved.repository.projectPath);
			const jobId = numberInput(input.id, "id");
			const response = await resolved.adapter.client.request<unknown>(
				"GET",
				`/api/v4/projects/${project}/jobs/${jobId}`,
				{ signal },
			);
			const job = objectValue(response.data, "GitLab job");
			if (job.status !== "manual") {
				throw new ForgeError("unsupported_capability", `GitLab job ${jobId} is not a playable manual job`, {
					provider: "gitlab",
					capability: "pipeline.manual_job",
				});
			}
			return;
		}
		const resource =
			toolName === "forge_issue"
				? "issue"
				: toolName === "forge_milestone"
					? "milestone"
					: toolName === "forge_change"
						? resolved.adapter instanceof GitLabAdapter && action === "approve"
							? "approval"
							: "change"
						: toolName === "forge_wiki"
							? "wiki"
							: toolName === "forge_pipeline"
								? action.includes("job")
									? "job"
									: "pipeline"
								: toolName === "forge_release"
									? "release"
									: action === "submit_review" || action === "prepare_merge" || action === "merge"
										? "change"
										: action === "prepare_release" || action === "release"
											? "release"
											: undefined;
		if (resource) return this.ensureResourceCapability(resolved, resource, signal);
		throw new ForgeError("unsupported_capability", `Workflow transition ${action} is not implemented yet`, {
			capability: `workflow.${action}`,
		});
	}

	private async mutate(
		resolved: ResolvedForge,
		toolName: ForgeToolName,
		action: string,
		input: Record<string, unknown>,
		signal: AbortSignal | undefined,
		context: ForgeToolContext,
	): Promise<unknown> {
		const project = resolved.repository.projectPath;
		const body = inputBody(input);
		const id = input.id;
		const policy = loadForgePolicy(context.cwd, context.projectTrusted);
		if (policy && toolName === "forge_change" && action === "create") {
			const facts = await localFacts(context.cwd);
			const labels = Array.isArray(body.labels)
				? body.labels.filter((label): label is string => typeof label === "string")
				: typeof body.labels === "string"
					? body.labels
							.split(",")
							.map((label) => label.trim())
							.filter(Boolean)
					: [];
			enforceForgePolicy(
				policy,
				{
					...facts,
					issueLinked: linkedIssue(`${String(body.title ?? "")}\n${String(body.description ?? body.body ?? "")}`),
					labels,
					targetBranch:
						typeof body.target_branch === "string"
							? body.target_branch
							: typeof body.base === "string"
								? body.base
								: undefined,
				},
				"submit_review",
			);
		}
		if ((toolName === "forge_change" && action === "merge") || toolName === "forge_release") {
			if (policy) {
				const stage: ForgePolicyStage = toolName === "forge_release" ? "release" : "merge";
				const remoteFacts =
					toolName === "forge_change"
						? await this.remoteMergeFacts(resolved, numberInput(id, "id"), policy, signal)
						: await this.remoteReleaseFacts(resolved, body, policy, signal);
				const requestedMergeMethod = body.merge_method;
				const mergeMethod =
					requestedMergeMethod === "merge" ||
					requestedMergeMethod === "squash" ||
					requestedMergeMethod === "rebase"
						? requestedMergeMethod
						: body.squash === true
							? "squash"
							: undefined;
				enforceForgePolicy(policy, { ...remoteFacts, mergeMethod }, stage);
			}
		}

		if (resolved.adapter instanceof GitLabAdapter) {
			return this.mutateGitLab(resolved.adapter, project, toolName, action, id, gitLabBody(body), signal);
		}
		return this.mutateGitHub(resolved.adapter, project, toolName, action, id, body, signal);
	}

	private async remoteMergeFacts(
		resolved: ResolvedForge,
		id: number,
		policy: ForgePolicy,
		signal?: AbortSignal,
	): Promise<ForgePolicyFacts> {
		const project = encodeURIComponent(resolved.repository.projectPath);
		if (resolved.adapter instanceof GitLabAdapter) {
			const mrResponse = await resolved.adapter.client.request<unknown>(
				"GET",
				`/api/v4/projects/${project}/merge_requests/${id}`,
				{ signal },
			);
			const mr = objectValue(mrResponse.data, "GitLab merge request");
			const facts: ForgePolicyFacts = {
				issueLinked: linkedIssue(`${String(mr.title ?? "")}\n${String(mr.description ?? "")}`),
				labels: Array.isArray(mr.labels)
					? mr.labels.filter((label): label is string => typeof label === "string")
					: [],
				targetBranch: typeof mr.target_branch === "string" ? mr.target_branch : undefined,
			};
			const requests: Promise<void>[] = [];
			if (policy.workflow.requiredApprovals > 0) {
				requests.push(
					resolved.adapter.client
						.request<unknown>("GET", `/api/v4/projects/${project}/merge_requests/${id}/approvals`, { signal })
						.then((response) => {
							const approvals = objectValue(response.data, "GitLab merge request approvals").approved_by;
							facts.approvals = Array.isArray(approvals) ? approvals.length : 0;
						}),
				);
			}
			if (policy.workflow.blockUnresolvedDiscussions) {
				requests.push(
					this.fetchPolicyArrayPages(
						resolved,
						`/api/v4/projects/${project}/merge_requests/${id}/discussions`,
						"GitLab merge request discussions",
						signal,
					).then((discussions) => {
						facts.unresolvedDiscussions = discussions.filter((discussion) => {
							const notes = objectValue(discussion, "GitLab discussion").notes;
							return (
								Array.isArray(notes) &&
								notes.some((note) => {
									const value = objectValue(note, "GitLab discussion note");
									return value.resolvable === true && value.resolved !== true;
								})
							);
						}).length;
					}),
				);
			}
			if (policy.workflow.requireSuccessfulPipeline) {
				requests.push(
					resolved.adapter.client
						.request<unknown>("GET", `/api/v4/projects/${project}/merge_requests/${id}/pipelines`, {
							query: { per_page: 1 },
							signal,
						})
						.then((response) => {
							const pipelines = arrayValue(response.data, "GitLab merge request pipelines");
							facts.pipelineSuccessful =
								pipelines.length > 0 &&
								objectValue(pipelines[0], "GitLab merge request pipeline").status === "success";
						}),
				);
			}
			await Promise.all(requests);
			return facts;
		}

		const githubProject = encodedGitHubProject(resolved.repository.projectPath);
		const prResponse = await resolved.adapter.client.request<unknown>("GET", `/repos/${githubProject}/pulls/${id}`, {
			signal,
		});
		const pr = objectValue(prResponse.data, "GitHub pull request");
		const labels = Array.isArray(pr.labels)
			? pr.labels.flatMap((label) => {
					const name = objectValue(label, "GitHub pull request label").name;
					return typeof name === "string" ? [name] : [];
				})
			: [];
		const facts: ForgePolicyFacts = {
			issueLinked: linkedIssue(`${String(pr.title ?? "")}\n${String(pr.body ?? "")}`),
			labels,
			targetBranch:
				typeof pr.base === "object" && pr.base !== null && typeof (pr.base as { ref?: unknown }).ref === "string"
					? (pr.base as { ref: string }).ref
					: undefined,
		};
		if (policy.workflow.requiredApprovals > 0) {
			const reviews = await this.fetchPolicyArrayPages(
				resolved,
				`/repos/${githubProject}/pulls/${id}/reviews`,
				"GitHub pull request reviews",
				signal,
			);
			const states = new Map<string, string>();
			for (const review of reviews) {
				const value = objectValue(review, "GitHub pull request review");
				const user =
					typeof value.user === "object" && value.user !== null
						? (value.user as { login?: unknown }).login
						: undefined;
				if (typeof user === "string" && typeof value.state === "string")
					states.set(user, value.state.toUpperCase());
			}
			facts.approvals = [...states.values()].filter((state) => state === "APPROVED").length;
		}
		if (policy.workflow.blockUnresolvedDiscussions) {
			// GitHub does not expose review-thread resolution through REST. Unknown fails the policy closed.
			facts.unresolvedDiscussions = undefined;
		}
		if (policy.workflow.requireSuccessfulPipeline) {
			const headSha =
				typeof pr.head === "object" && pr.head !== null && typeof (pr.head as { sha?: unknown }).sha === "string"
					? (pr.head as { sha: string }).sha
					: undefined;
			if (headSha) {
				const [statusResponse, checkRuns] = await Promise.all([
					resolved.adapter.client.request<unknown>("GET", `/repos/${githubProject}/commits/${headSha}/status`, {
						signal,
					}),
					this.fetchGitHubCheckRunPages(resolved, `/repos/${githubProject}/commits/${headSha}/check-runs`, signal),
				]);
				const status = objectValue(statusResponse.data, "GitHub combined commit status");
				const statuses = Array.isArray(status.statuses) ? status.statuses : [];
				const checksSuccessful = checkRuns.every((run) => {
					const value = objectValue(run, "GitHub check run");
					return (
						value.status === "completed" &&
						(value.conclusion === "success" || value.conclusion === "neutral" || value.conclusion === "skipped")
					);
				});
				facts.pipelineSuccessful =
					(statuses.length > 0 || checkRuns.length > 0) &&
					(statuses.length === 0 || status.state === "success") &&
					checksSuccessful;
			}
		}
		return facts;
	}

	private async remoteReleaseFacts(
		resolved: ResolvedForge,
		body: Record<string, unknown>,
		policy: ForgePolicy,
		signal?: AbortSignal,
	): Promise<ForgePolicyFacts> {
		const facts: ForgePolicyFacts = {};
		const ref =
			typeof body.ref === "string"
				? body.ref
				: typeof body.target_commitish === "string"
					? body.target_commitish
					: undefined;
		if (policy.workflow.release.requireTagFromProtectedBranch && ref) {
			const project =
				resolved.adapter instanceof GitLabAdapter
					? encodeURIComponent(resolved.repository.projectPath)
					: encodedGitHubProject(resolved.repository.projectPath);
			try {
				if (resolved.adapter instanceof GitLabAdapter) {
					await resolved.adapter.client.request(
						"GET",
						`/api/v4/projects/${project}/protected_branches/${encodeURIComponent(ref)}`,
						{ signal },
					);
					facts.tagFromProtectedBranch = true;
				} else {
					const response = await resolved.adapter.client.request<unknown>(
						"GET",
						`/repos/${project}/branches/${encodeURIComponent(ref)}`,
						{ signal },
					);
					facts.tagFromProtectedBranch = objectValue(response.data, "GitHub branch").protected === true;
				}
			} catch (error) {
				if (!(error instanceof ForgeError) || error.code !== "not_found") throw error;
				facts.tagFromProtectedBranch = false;
			}
		}
		if (policy.workflow.release.requireMilestoneClosed && resolved.adapter instanceof GitLabAdapter) {
			const milestoneTitles = Array.isArray(body.milestones)
				? body.milestones.filter((value): value is string => typeof value === "string")
				: [];
			if (milestoneTitles.length > 0) {
				const page = await resolved.adapter.listMilestones(
					resolved.repository.projectPath,
					{ state: "closed" },
					signal,
				);
				const closed = new Set(page.items.flatMap((item) => (typeof item.title === "string" ? [item.title] : [])));
				facts.milestoneClosed = milestoneTitles.every((title) => closed.has(title));
			}
		}
		if (policy.workflow.requireSuccessfulPipeline && ref && resolved.adapter instanceof GitLabAdapter) {
			const page = await resolved.adapter.listPipelines(
				resolved.repository.projectPath,
				{ ref, per_page: 1 },
				signal,
			);
			facts.pipelineSuccessful = page.items[0]?.status === "success";
		}
		return facts;
	}

	private mutateGitLab(
		adapter: GitLabAdapter,
		project: string,
		toolName: ForgeToolName,
		action: string,
		id: unknown,
		body: GitLabWriteBody,
		signal?: AbortSignal,
	): Promise<unknown> {
		const encoded = encodeURIComponent(project);
		if (toolName === "forge_issue") {
			if (action === "create") return adapter.createIssue(project, body, signal);
			if (action === "comment")
				return adapter.client
					.request("POST", `/api/v4/projects/${encoded}/issues/${numberInput(id, "id")}/notes`, { body, signal })
					.then((response) => response.data);
			const update =
				action === "close"
					? { ...body, state_event: "close" }
					: action === "reopen"
						? { ...body, state_event: "reopen" }
						: body;
			if (action === "update" || action === "close" || action === "reopen")
				return adapter.updateIssue(project, numberInput(id, "id"), update, signal);
		}
		if (toolName === "forge_milestone") {
			if (action === "create") return adapter.createMilestone(project, body, signal);
			const update =
				action === "close"
					? { ...body, state_event: "close" }
					: action === "reopen"
						? { ...body, state_event: "activate" }
						: body;
			if (action === "update" || action === "close" || action === "reopen")
				return adapter.updateMilestone(project, numberInput(id, "id"), update, signal);
		}
		if (toolName === "forge_change") {
			if (action === "create") return adapter.createMergeRequest(project, body, signal);
			if (action === "update") return adapter.updateMergeRequest(project, numberInput(id, "id"), body, signal);
			if (action === "merge") return adapter.mergeMergeRequest(project, numberInput(id, "id"), body, signal);
			if (action === "approve")
				return adapter.client
					.request("POST", `/api/v4/projects/${encoded}/merge_requests/${numberInput(id, "id")}/approve`, {
						body,
						signal,
					})
					.then((response) => response.data);
			if (action === "comment")
				return adapter.client
					.request("POST", `/api/v4/projects/${encoded}/merge_requests/${numberInput(id, "id")}/notes`, {
						body,
						signal,
					})
					.then((response) => response.data);
		}
		if (toolName === "forge_wiki") {
			if (action === "create") return adapter.createWikiPage(project, body, signal);
			if (action === "update")
				return adapter.updateWikiPage(project, stringInput(body.slug ?? id, "slug"), body, signal);
			if (action === "delete") return adapter.deleteWikiPage(project, stringInput(body.slug ?? id, "slug"), signal);
		}
		if (toolName === "forge_pipeline") {
			if (action === "trigger") return adapter.createPipeline(project, body, signal);
			if (action === "retry") return adapter.retryPipeline(project, numberInput(id, "id"), signal);
			if (action === "cancel") return adapter.cancelPipeline(project, numberInput(id, "id"), signal);
			if (action === "play_job") return adapter.playJob(project, numberInput(id, "id"), body, signal);
			if (action === "retry_job") return adapter.retryJob(project, numberInput(id, "id"), signal);
			if (action === "cancel_job") return adapter.cancelJob(project, numberInput(id, "id"), signal);
		}
		if (toolName === "forge_release") {
			if (action !== "create" && action !== "update") {
				throw new ForgeError("validation_failed", `Unsupported GitLab release action: ${action}`);
			}
			const tag = action === "create" ? undefined : stringInput(id ?? body.tag_name, "id");
			const path =
				action === "create"
					? `/api/v4/projects/${encoded}/releases`
					: `/api/v4/projects/${encoded}/releases/${encodeURIComponent(tag ?? "")}`;
			return adapter.client
				.request(action === "create" ? "POST" : "PUT", path, { body, signal })
				.then((response) => response.data);
		}
		throw new ForgeError("validation_failed", `Unsupported GitLab ${toolName} action: ${action}`);
	}

	private mutateGitHub(
		adapter: GitHubAdapter,
		project: string,
		toolName: ForgeToolName,
		action: string,
		id: unknown,
		body: GitHubWriteBody,
		signal?: AbortSignal,
	): Promise<unknown> {
		if (toolName === "forge_issue") {
			if (action === "create") return adapter.createIssue(project, body, signal);
			if (action === "comment")
				return adapter.client
					.request("POST", `/repos/${encodedGitHubProject(project)}/issues/${numberInput(id, "id")}/comments`, {
						body,
						signal,
					})
					.then((response) => response.data);
			if (action === "update" || action === "close" || action === "reopen") {
				const update =
					action === "close"
						? { ...body, state: "closed" }
						: action === "reopen"
							? { ...body, state: "open" }
							: body;
				return adapter.updateIssue(project, numberInput(id, "id"), update, signal);
			}
		}
		if (toolName === "forge_milestone") {
			if (action === "create") return adapter.createMilestone(project, body, signal);
			const update =
				action === "close" ? { ...body, state: "closed" } : action === "reopen" ? { ...body, state: "open" } : body;
			return adapter.updateMilestone(project, numberInput(id, "id"), update, signal);
		}
		if (toolName === "forge_change") {
			if (action === "create") return adapter.createPullRequest(project, body, signal);
			if (action === "update") return adapter.updatePullRequest(project, numberInput(id, "id"), body, signal);
			if (action === "merge") return adapter.mergePullRequest(project, numberInput(id, "id"), body, signal);
			if (action === "approve")
				return adapter.client
					.request("POST", `/repos/${encodedGitHubProject(project)}/pulls/${numberInput(id, "id")}/reviews`, {
						body: { ...body, event: "APPROVE" },
						signal,
					})
					.then((response) => response.data);
			if (action === "comment")
				return adapter.client
					.request("POST", `/repos/${encodedGitHubProject(project)}/issues/${numberInput(id, "id")}/comments`, {
						body,
						signal,
					})
					.then((response) => response.data);
		}
		if (toolName === "forge_pipeline") {
			if (action === "retry") return adapter.rerunWorkflow(project, numberInput(id, "id"), signal);
			if (action === "cancel") return adapter.cancelWorkflow(project, numberInput(id, "id"), signal);
		}
		if (toolName === "forge_release") {
			if (action === "create") return adapter.createRelease(project, body, signal);
			if (action === "update") return adapter.updateRelease(project, numberInput(id, "id"), body, signal);
			throw new ForgeError("validation_failed", `Unsupported GitHub release action: ${action}`);
		}
		if (toolName === "forge_wiki") {
			throw new ForgeError("unsupported_capability", "GitHub Wiki does not have a supported REST API", {
				capability: "wiki",
			});
		}
		throw new ForgeError("validation_failed", `Unsupported GitHub ${toolName} action: ${action}`);
	}

	private async transition(
		resolved: ResolvedForge,
		transition: string,
		input: Record<string, unknown>,
		signal: AbortSignal | undefined,
		context: ForgeToolContext,
	): Promise<unknown> {
		if (transition === "prepare_merge" || transition === "prepare_release") {
			return this.audit(
				resolved,
				{ ...input, workflow: transition === "prepare_merge" ? "merge" : "release" },
				context,
				signal,
			);
		}
		if (transition === "merge") {
			return this.mutate(
				resolved,
				"forge_change",
				"merge",
				{ ...input, id: input.changeId, input: inputBody(input) },
				signal,
				context,
			);
		}
		if (transition === "submit_review") {
			return this.mutate(resolved, "forge_change", "create", input, signal, context);
		}
		if (transition === "release") {
			return this.mutate(
				resolved,
				"forge_release",
				"create",
				{ ...input, id: undefined, input: inputBody(input) },
				signal,
				context,
			);
		}
		throw new ForgeError("unsupported_capability", `Workflow transition ${transition} is not implemented yet`, {
			capability: `workflow.${transition}`,
		});
	}
}

export function createForgeService(options: CreateForgeServiceOptions): ForgeService {
	return new DefaultForgeService(options);
}
