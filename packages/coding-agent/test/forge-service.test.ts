import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { getGlobalDispatcher, MockAgent, setGlobalDispatcher } from "undici-client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createForgeService } from "../src/core/forge/service.ts";
import type { ForgeToolContext } from "../src/core/forge/tools.ts";

const originalDispatcher = getGlobalDispatcher();
const tempDirs: string[] = [];
let dispatcher: MockAgent;

beforeEach(() => {
	dispatcher = new MockAgent();
	dispatcher.disableNetConnect();
	setGlobalDispatcher(dispatcher);
});

afterEach(async () => {
	setGlobalDispatcher(originalDispatcher);
	await dispatcher.close();
	vi.unstubAllEnvs();
	for (const path of tempDirs.splice(0)) rmSync(path, { recursive: true, force: true });
});

function harness(confirmation = false) {
	const root = mkdtempSync(join(tmpdir(), "bone-forge-service-"));
	const agentDir = join(root, "agent");
	mkdirSync(agentDir, { recursive: true });
	tempDirs.push(root);
	const confirm = vi.fn(async () => confirmation);
	const context: ForgeToolContext = {
		cwd: root,
		agentDir,
		toolCallId: "tool-1",
		interactive: true,
		projectTrusted: true,
		confirm,
	};
	return { service: createForgeService({ cwd: root, agentDir, dispatcher }), context, confirm };
}

describe("DefaultForgeService", () => {
	it("rejects a bracketed IPv6 loopback API before making a request", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const { service, context } = harness();
		writeFileSync(
			join(context.agentDir, "forge.json"),
			JSON.stringify({
				version: 1,
				instances: [
					{
						provider: "gitlab",
						host: "[::1]",
						apiBaseUrl: "https://[::1]",
						allowPrivateNetwork: false,
					},
				],
			}),
		);

		await expect(
			service.execute("forge_context", { remote: "https://[::1]/acme/widget.git" }, undefined, context),
		).rejects.toMatchObject({ code: "unsafe_remote" });
	});

	it("resolves public GitHub context and negotiates capabilities", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/user" }).reply(200, {
			id: 9,
			login: "octo",
			name: null,
			html_url: "https://github.com/octo",
			avatar_url: "https://avatars.example/octo",
			followers_url: "https://api.github.com/users/octo/followers",
			events_url: "https://api.github.com/users/octo/events",
			public_repos: 99,
			token_echo: "service-secret",
		});
		for (const path of [
			"/repos/acme/widget/issues?per_page=1",
			"/repos/acme/widget/milestones?per_page=1",
			"/repos/acme/widget/pulls?per_page=1",
			"/repos/acme/widget/actions/runs?per_page=1",
			"/repos/acme/widget/releases?per_page=1",
		]) {
			api.intercept({ method: "GET", path }).reply(200, path.includes("actions/runs") ? { workflow_runs: [] } : []);
		}
		const { service, context } = harness();

		const result = await service.execute(
			"forge_context",
			{ remote: "https://github.com/acme/widget.git" },
			undefined,
			context,
		);

		expect(result).toMatchObject({
			repository: { provider: "github", host: "github.com", projectPath: "acme/widget" },
			user: { id: 9, login: "octo", name: null, htmlUrl: "https://github.com/octo" },
		});
		if (typeof result !== "object" || result === null || !("user" in result)) throw new Error("Expected Forge user");
		expect(result.user).toEqual({ id: 9, login: "octo", name: null, htmlUrl: "https://github.com/octo" });
		expect(JSON.stringify(result)).not.toMatch(
			/service-secret|avatar_url|followers_url|events_url|public_repos|token_echo/,
		);
	});

	it("keeps release queries strict after a status-only capability probe", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/releases?per_page=1" }).reply(200, "probe body");
		api.intercept({ method: "GET", path: "/repos/acme/widget/releases?per_page=10" }).reply(200, "not json");
		const { service, context } = harness();

		await expect(
			service.execute(
				"forge_query",
				{ remote: "https://github.com/acme/widget.git", operation: "list", resource: "release" },
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "invalid_remote_response" });
	});

	it("rejects an empty successful GitLab release write response", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/releases?per_page=1" }).reply(200, []);
		api.intercept({ method: "POST", path: "/api/v4/projects/acme%2Fwidget/releases" }).reply(201, "");
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_release",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "create",
					requestId: "release-empty-response",
					input: { tag_name: "v1.0.0" },
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "invalid_remote_response" });
	});

	it("returns compact issue summaries with a default limit of 10", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/issues?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/issues?per_page=10" }).reply(
			200,
			[
				{
					id: 70,
					iid: 7,
					title: "Compact issue",
					state: "opened",
					description: `${"Relevant preview text. ".repeat(40)}BODY_TAIL_SENTINEL`,
					author: { username: "alice", avatar_url: "RAW_SENTINEL" },
					raw: { nested: "RAW_SENTINEL" },
				},
			],
			{ headers: { "x-next-page": "2" } },
		);
		const { service, context } = harness();

		const result = await service.execute(
			"forge_query",
			{ remote: "https://gitlab.com/acme/widget.git", operation: "list", resource: "issue" },
			undefined,
			context,
		);

		expect(result).toMatchObject({
			resource: "issue",
			mode: "list",
			returned: 1,
			hasMore: true,
			nextCursor: "2",
			items: [
				{
					id: 70,
					number: 7,
					title: "Compact issue",
					author: "alice",
					bodyPreviewTruncated: true,
				},
			],
		});
		const serialized = JSON.stringify(result);
		expect(serialized).toContain("Relevant preview text");
		expect(serialized).not.toMatch(/BODY_TAIL_SENTINEL|RAW_SENTINEL|description|avatar_url/);
	});

	it("uses GitHub Search API for issue body searches", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/issues?per_page=1" }).reply(200, []);
		api.intercept({
			method: "GET",
			path: "/search/issues?per_page=10&q=database+deadlock+repo%3Aacme%2Fwidget+is%3Aissue+is%3Aopen",
		}).reply(200, {
			total_count: 1,
			items: [{ id: 70, number: 7, title: "Database deadlock", body: "Relevant issue body" }],
		});
		const { service, context } = harness();

		await expect(
			service.execute(
				"forge_query",
				{
					remote: "https://github.com/acme/widget.git",
					resource: "issue",
					operation: "list",
					search: "database deadlock",
					state: "open",
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({
			items: [{ number: 7, title: "Database deadlock", bodyPreview: "Relevant issue body" }],
		});
	});

	it("maps provider-neutral pipeline states to provider status filters", async () => {
		vi.stubEnv("GITLAB_TOKEN", "gitlab-secret");
		vi.stubEnv("GITHUB_TOKEN", "github-secret");
		const gitlab = dispatcher.get("https://gitlab.com");
		gitlab.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/pipelines?per_page=1" }).reply(200, []);
		gitlab
			.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/pipelines?per_page=10&status=failed" })
			.reply(200, []);
		const github = dispatcher.get("https://api.github.com");
		github.intercept({ method: "GET", path: "/repos/acme/widget/actions/runs?per_page=1" }).reply(200, {
			workflow_runs: [],
		});
		github
			.intercept({ method: "GET", path: "/repos/acme/widget/actions/runs?per_page=10&status=failure" })
			.reply(200, { workflow_runs: [] });
		const { service, context } = harness();

		await expect(
			service.execute(
				"forge_query",
				{
					remote: "https://gitlab.com/acme/widget.git",
					operation: "list",
					resource: "pipeline",
					state: "failed",
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ resource: "pipeline", items: [] });
		await expect(
			service.execute(
				"forge_query",
				{
					remote: "https://github.com/acme/widget.git",
					operation: "list",
					resource: "pipeline",
					state: "failed",
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ resource: "pipeline", items: [] });
	});

	it("retrieves at most five issue details in a bounded batch", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/issues?per_page=1" }).reply(200, []);
		for (const id of [7, 8]) {
			api.intercept({ method: "GET", path: `/api/v4/projects/acme%2Fwidget/issues/${id}` }).reply(200, {
				id: id * 10,
				iid: id,
				title: `Issue ${id}`,
				description: "x".repeat(10_000),
			});
		}
		const { service, context } = harness();

		const result = await service.execute(
			"forge_query",
			{ remote: "https://gitlab.com/acme/widget.git", operation: "get_many", resource: "issue", ids: [7, 8] },
			undefined,
			context,
		);
		if (typeof result !== "object" || result === null || !("items" in result) || !Array.isArray(result.items)) {
			throw new Error("Expected Forge batch result");
		}

		expect(result).toMatchObject({ mode: "batch", returned: 2 });
		for (const item of result.items) {
			if (typeof item !== "object" || item === null || !("body" in item) || typeof item.body !== "string") {
				throw new Error("Expected bounded batch body");
			}
			expect(Buffer.byteLength(item.body, "utf8")).toBeLessThanOrEqual(8 * 1024);
		}
	});

	it("rejects oversized batches and GitHub repository-scope search qualifiers before network access", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const { service, context } = harness();
		const common = { remote: "https://github.com/acme/widget.git", operation: "list", resource: "issue" };

		await expect(
			service.execute(
				"forge_query",
				{ ...common, operation: "get_many", ids: [1, 2, 3, 4, 5, 6] },
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "validation_failed" });
		await expect(
			service.execute("forge_query", { ...common, search: "deadlock repo:another/project" }, undefined, context),
		).rejects.toMatchObject({ code: "validation_failed" });
	});

	it("uses a detail endpoint and bounds multibyte issue bodies to 16 KiB", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/issues?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/repos/acme/widget/issues/7" }).reply(200, {
			id: 70,
			number: 7,
			title: "Detailed issue",
			body: "界".repeat(20_000),
			raw: "RAW_SENTINEL",
		});
		const { service, context } = harness();

		const result = await service.execute(
			"forge_query",
			{ remote: "https://github.com/acme/widget.git", operation: "get", resource: "issue", id: 7 },
			undefined,
			context,
		);
		const serialized = JSON.stringify(result);
		const item = (result as { item: { body: string; bodyTruncated: boolean; bodyOriginalBytes: number } }).item;

		expect(Buffer.byteLength(item.body, "utf8")).toBeLessThanOrEqual(16 * 1024);
		expect(item).toMatchObject({ bodyTruncated: true, bodyOriginalBytes: 60_000 });
		expect(serialized).not.toContain("RAW_SENTINEL");
	});

	it("rejects query limits above 50 before sending a list request", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const { service, context } = harness();

		await expect(
			service.execute(
				"forge_query",
				{ remote: "https://gitlab.com/acme/widget.git", operation: "list", resource: "issue", limit: 51 },
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "validation_failed" });
	});

	it("replays a completed mutation without sending a duplicate request", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/issues?per_page=1" }).reply(200, []);
		api.intercept({ method: "POST", path: "/repos/acme/widget/issues" }).reply(201, {
			id: 31,
			number: 12,
			title: "Tracked issue",
		});
		const { service, context } = harness();
		const input = {
			remote: "https://github.com/acme/widget.git",
			action: "create",
			requestId: "request-1",
			input: { title: "Tracked issue" },
		};

		const first = await service.execute("forge_issue", input, undefined, context);
		const replay = await service.execute("forge_issue", input, undefined, context);

		expect(first).toMatchObject({ id: 31, number: 12 });
		expect(replay).toEqual(first);
	});

	it("classifies reuse of a request id for a different mutation as a Forge conflict", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/issues?per_page=1" }).reply(200, []);
		api.intercept({ method: "POST", path: "/repos/acme/widget/issues" }).reply(201, {
			id: 31,
			number: 12,
			title: "First intent",
		});
		const { service, context } = harness();
		const common = {
			remote: "https://github.com/acme/widget.git",
			action: "create",
			requestId: "conflicting-request",
		};

		await service.execute("forge_issue", { ...common, input: { title: "First intent" } }, undefined, context);
		await expect(
			service.execute("forge_issue", { ...common, input: { title: "Different intent" } }, undefined, context),
		).rejects.toMatchObject({ code: "conflict" });
	});

	it("maps normalized issue content to GitHub request fields", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/issues?per_page=1" }).reply(200, []);
		api.intercept({
			method: "POST",
			path: "/repos/acme/widget/issues",
			body: JSON.stringify({
				title: "Agent-friendly contract",
				body: "Normalized body",
				labels: ["tooling"],
				assignees: ["octo"],
				milestone: 3,
			}),
		}).reply(201, {
			id: 31,
			number: 12,
			title: "Agent-friendly contract",
			body: "RAW_MUTATION_BODY_SENTINEL",
			user: { login: "octo", token: "RAW_MUTATION_USER_SENTINEL" },
		});
		const { service, context } = harness();

		await expect(
			service.execute(
				"forge_issue",
				{
					remote: "https://github.com/acme/widget.git",
					action: "create",
					requestId: "normalized-issue",
					input: {
						title: "Agent-friendly contract",
						body: "Normalized body",
						labels: ["tooling"],
						assignees: ["octo"],
						milestoneNumber: 3,
					},
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ ok: true, resource: "issue", number: 12 });
		const journal = readFileSync(join(context.agentDir, "forge-mutations.json"), "utf8");
		expect(journal).not.toMatch(/RAW_MUTATION_BODY_SENTINEL|RAW_MUTATION_USER_SENTINEL|"body"|"user"/);
	});

	it("resolves GitLab assignee usernames instead of silently dropping them", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/issues?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/api/v4/users?username=alice" }).reply(200, [
			{ id: 17, username: "alice" },
		]);
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/milestones?iids%5B%5D=3&per_page=2" }).reply(
			200,
			[{ id: 103, iid: 3, title: "Version 1" }],
		);
		api.intercept({
			method: "POST",
			path: "/api/v4/projects/acme%2Fwidget/issues",
			body: JSON.stringify({ title: "Assigned issue", milestone_id: 103, assignee_ids: [17] }),
		}).reply(201, { id: 31, iid: 12, title: "Assigned issue" });
		const { service, context } = harness();

		await expect(
			service.execute(
				"forge_issue",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "create",
					requestId: "assigned-issue",
					input: { title: "Assigned issue", assignees: ["alice"], milestoneNumber: 3 },
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ ok: true, resource: "issue", number: 12 });
	});

	it("maps normalized change branches to GitLab merge request fields", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/merge_requests?per_page=1" }).reply(200, []);
		api.intercept({
			method: "POST",
			path: "/api/v4/projects/acme%2Fwidget/merge_requests",
			body: JSON.stringify({
				title: "Draft: Normalize branches",
				description: "Portable input",
				source_branch: "feature/contracts",
				target_branch: "main",
			}),
		}).reply(201, { id: 70, iid: 7, title: "Normalize branches" });
		const { service, context } = harness();

		await expect(
			service.execute(
				"forge_change",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "create",
					requestId: "normalized-change",
					input: {
						title: "Normalize branches",
						body: "Portable input",
						sourceBranch: "feature/contracts",
						targetBranch: "main",
						draft: true,
					},
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ ok: true, resource: "change", number: 7 });
	});

	it("maps provider-neutral squash merges to the GitLab merge API", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/merge_requests?per_page=1" }).reply(200, []);
		api.intercept({
			method: "PUT",
			path: "/api/v4/projects/acme%2Fwidget/merge_requests/7/merge",
			body: JSON.stringify({ squash: true }),
		}).reply(200, { id: 70, iid: 7, state: "merged" });
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_change",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "merge",
					id: 7,
					requestId: "merge-squash-7",
					input: { mergeMethod: "squash" },
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ ok: true, resource: "change", number: 7, state: "merged" });
	});

	it("maps normalized pipeline variables to GitLab key-value entries", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/pipelines?per_page=1" }).reply(200, []);
		api.intercept({
			method: "POST",
			path: "/api/v4/projects/acme%2Fwidget/pipeline",
			body: JSON.stringify({ ref: "main", variables: [{ key: "DEPLOY_ENV", value: "staging" }] }),
		}).reply(201, { id: 42, status: "pending", ref: "main" });
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_pipeline",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "trigger",
					requestId: "pipeline-trigger",
					input: { ref: "main", variables: [{ name: "DEPLOY_ENV", value: "staging" }] },
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ id: 42 });
	});

	it("resolves a GitHub release tag before updating the numeric release endpoint", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/releases?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/repos/acme/widget/releases/tags/v1.2.0" }).reply(200, {
			id: 91,
			tag_name: "v1.2.0",
		});
		api.intercept({
			method: "PATCH",
			path: "/repos/acme/widget/releases/91",
			body: JSON.stringify({ tag_name: "v1.2.0", name: "Version 1.2", body: "Release notes" }),
		}).reply(200, { id: 91, tag_name: "v1.2.0", name: "Version 1.2" });
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_release",
				{
					remote: "https://github.com/acme/widget.git",
					action: "update",
					id: "v1.2.0",
					requestId: "release-update-tag",
					input: { name: "Version 1.2", body: "Release notes" },
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ id: 91, key: "v1.2.0" });
	});

	it("rejects release fields that the resolved provider cannot preserve", async () => {
		vi.stubEnv("GITLAB_TOKEN", "gitlab-secret");
		vi.stubEnv("GITHUB_TOKEN", "github-secret");
		dispatcher
			.get("https://gitlab.com")
			.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/releases?per_page=1" })
			.reply(200, []);
		dispatcher
			.get("https://api.github.com")
			.intercept({ method: "GET", path: "/repos/acme/widget/releases?per_page=1" })
			.reply(200, []);
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_release",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "create",
					id: "v1.0.0",
					requestId: "gitlab-draft",
					input: { draft: true },
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "unsupported_capability" });
		await expect(
			service.execute(
				"forge_release",
				{
					remote: "https://github.com/acme/widget.git",
					action: "create",
					id: "v1.0.0",
					requestId: "github-milestone",
					input: { milestoneTitles: ["Version 1"] },
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "unsupported_capability" });
	});

	it("maps GitHub milestone close to the remote closed state", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/milestones?per_page=1" }).reply(200, []);
		api.intercept({
			method: "PATCH",
			path: "/repos/acme/widget/milestones/3",
			body: JSON.stringify({ state: "closed" }),
		}).reply(200, { id: 3, number: 3, state: "closed" });
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_milestone",
				{
					remote: "https://github.com/acme/widget.git",
					action: "close",
					id: 3,
					requestId: "close-milestone-3",
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ state: "closed" });
	});

	it("resolves a GitLab milestone number before updating the global milestone id", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/milestones?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/milestones?iids%5B%5D=3&per_page=2" }).reply(
			200,
			[{ id: 103, iid: 3, title: "Version 1" }],
		);
		api.intercept({
			method: "PUT",
			path: "/api/v4/projects/acme%2Fwidget/milestones/103",
			body: JSON.stringify({ state_event: "close" }),
		}).reply(200, { id: 103, iid: 3, state: "closed" });
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_milestone",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "close",
					id: 3,
					requestId: "close-gitlab-milestone-3",
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ resource: "milestone", number: 3, state: "closed" });
	});

	it("rejects a GitLab approval body before confirmation", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const { service, context, confirm } = harness(true);

		await expect(
			service.execute(
				"forge_change",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "approve",
					id: 7,
					requestId: "approve-with-body",
					input: { body: "Approved" },
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "unsupported_capability" });
		expect(confirm).not.toHaveBeenCalled();
	});

	it("supports GitHub job reruns and returns identifiers for empty mutation responses", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/actions/runs?per_page=1" }).reply(200, {
			workflow_runs: [],
		});
		api.intercept({ method: "POST", path: "/repos/acme/widget/actions/jobs/9/rerun" }).reply(201, "");
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_pipeline",
				{
					remote: "https://github.com/acme/widget.git",
					action: "retry_job",
					id: 9,
					requestId: "retry-job-9",
				},
				undefined,
				context,
			),
		).resolves.toEqual({ ok: true, resource: "job", action: "retry_job", id: 9 });
	});

	it("rejects unsupported GitHub pipeline actions before confirmation", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const { service, context, confirm } = harness(true);

		await expect(
			service.execute(
				"forge_pipeline",
				{
					remote: "https://github.com/acme/widget.git",
					action: "trigger",
					requestId: "github-trigger",
					input: { ref: "main" },
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "unsupported_capability" });
		expect(confirm).not.toHaveBeenCalled();
	});

	it("allows read-only prepare transitions without a request id", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		dispatcher
			.get("https://api.github.com")
			.intercept({ method: "GET", path: "/repos/acme/widget/pulls?per_page=1" })
			.reply(200, []);
		const { service, context, confirm } = harness();

		await expect(
			service.execute(
				"forge_transition",
				{
					remote: "https://github.com/acme/widget.git",
					transition: "prepare_merge",
					changeId: 7,
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ compliant: true, policy: "not_configured" });
		expect(confirm).not.toHaveBeenCalled();
	});

	it("returns the change number for a GitHub transition merge response without identifiers", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls?per_page=1" }).reply(200, []);
		api.intercept({
			method: "PUT",
			path: "/repos/acme/widget/pulls/7/merge",
			body: JSON.stringify({ merge_method: "squash" }),
		}).reply(200, { sha: "abc123", merged: true, message: "Pull Request successfully merged" });
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_transition",
				{
					remote: "https://github.com/acme/widget.git",
					transition: "merge",
					changeId: 7,
					requestId: "transition-merge-7",
					input: { mergeMethod: "squash" },
				},
				undefined,
				context,
			),
		).resolves.toEqual({ ok: true, resource: "change", action: "merge", number: 7 });
	});

	it("does not execute a destructive mutation when confirmation is declined", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		dispatcher
			.get("https://api.github.com")
			.intercept({ method: "GET", path: "/repos/acme/widget/actions/runs?per_page=1" })
			.reply(200, { workflow_runs: [] });
		const { service, context, confirm } = harness();

		await expect(
			service.execute(
				"forge_pipeline",
				{
					remote: "https://github.com/acme/widget.git",
					action: "cancel",
					id: 42,
					requestId: "request-cancel",
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "approval_required" });
		expect(confirm).toHaveBeenCalledOnce();
	});

	it("re-fetches remote merge facts and blocks an under-approved pull request", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls/7" }).reply(200, {
			id: 70,
			number: 7,
			title: "Fixes #12",
			body: "Ready",
			labels: [],
			base: { ref: "main" },
			head: { sha: "abc123" },
		});
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls/7/reviews?per_page=100&page=1" }).reply(200, [
			{ id: 1, state: "APPROVED", user: { login: "reviewer-1" } },
		]);
		api.intercept({ method: "GET", path: "/repos/acme/widget/commits/abc123/status" }).reply(200, {
			state: "success",
			statuses: [{ id: 91, state: "success" }],
		});
		api.intercept({ method: "GET", path: "/repos/acme/widget/commits/abc123/check-runs?per_page=100&page=1" }).reply(
			200,
			{
				check_runs: [{ id: 92, status: "completed", conclusion: "success" }],
			},
		);
		const { service, context } = harness(true);
		mkdirSync(join(context.cwd, ".bone"), { recursive: true });
		writeFileSync(
			join(context.cwd, ".bone", "forge.yaml"),
			[
				"version: 1",
				"provider: github",
				"workflow:",
				"  requiredApprovals: 2",
				"  requireSuccessfulPipeline: true",
				"  protectedTargets: [main]",
			].join("\n"),
		);

		await expect(
			service.execute(
				"forge_change",
				{
					remote: "https://github.com/acme/widget.git",
					action: "merge",
					id: 7,
					requestId: "merge-7",
					input: { merge_method: "squash" },
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "policy_denied" });
	});

	it("preserves string Wiki slugs and GitLab release tags", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/wikis?per_page=1" }).reply(200, []);
		api.intercept({ method: "PUT", path: "/api/v4/projects/acme%2Fwidget/wikis/operations%2Frunbook" }).reply(200, {
			slug: "operations/runbook",
			title: "Runbook",
		});
		api.intercept({ method: "PUT", path: "/api/v4/projects/acme%2Fwidget/releases/v1.2.0" }).reply(200, {
			tag_name: "v1.2.0",
			name: "Version 1.2.0",
		});
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/releases?per_page=1" }).reply(200, []);
		const { service, context } = harness();

		await expect(
			service.execute(
				"forge_wiki",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "update",
					id: "operations/runbook",
					requestId: "wiki-update",
					input: { title: "Runbook", content: "Updated" },
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ key: "operations/runbook" });
		await expect(
			service.execute(
				"forge_release",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "update",
					id: "v1.2.0",
					requestId: "release-update",
					input: { name: "Version 1.2.0" },
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ key: "v1.2.0" });
	});

	it("journals mutating workflow transitions", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/releases?per_page=1" }).reply(200, []);
		api.intercept({
			method: "POST",
			path: "/api/v4/projects/acme%2Fwidget/releases",
			body: JSON.stringify({ tag_name: "v2.0.0", name: "Version 2", milestones: ["Version 2"] }),
		}).reply(201, {
			tag_name: "v2.0.0",
			name: "Version 2",
		});
		const { service, context } = harness(true);
		const input = {
			remote: "https://gitlab.com/acme/widget.git",
			transition: "release",
			requestId: "transition-release",
			input: { tagName: "v2.0.0", name: "Version 2", milestoneTitles: ["Version 2"] },
		};

		const first = await service.execute("forge_transition", input, undefined, context);
		const replay = await service.execute("forge_transition", input, undefined, context);

		expect(replay).toEqual(first);
	});

	it("watches GitLab pipelines and jobs through direct resource endpoints", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/pipelines?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/pipelines/4" }).reply(200, {
			id: 4,
			status: "success",
		});
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/jobs?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/jobs/9" }).reply(200, {
			id: 9,
			status: "failed",
		});
		const { service, context } = harness();
		const common = { remote: "https://gitlab.com/acme/widget.git", timeoutSeconds: 1 };

		await expect(
			service.execute(
				"forge_watch",
				{ ...common, resource: "pipeline", id: 4, until: ["success"] },
				undefined,
				context,
			),
		).resolves.toMatchObject({ state: "success", item: { id: 4 } });
		await expect(
			service.execute("forge_watch", { ...common, resource: "job", id: 9, until: ["failed"] }, undefined, context),
		).resolves.toMatchObject({ state: "failed", item: { id: 9 } });
	});

	it("blocks writes when the repository provider violates policy", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		dispatcher
			.get("https://api.github.com")
			.intercept({ method: "GET", path: "/repos/acme/widget/issues?per_page=1" })
			.reply(200, []);
		const { service, context } = harness(true);
		mkdirSync(join(context.cwd, ".bone"), { recursive: true });
		writeFileSync(join(context.cwd, ".bone", "forge.yaml"), "version: 1\nprovider: gitlab\n");

		await expect(
			service.execute(
				"forge_issue",
				{
					remote: "https://github.com/acme/widget.git",
					action: "create",
					requestId: "provider-mismatch",
					input: { title: "Must not be created" },
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "policy_denied" });
	});

	it("fails closed when GitLab MR approvals are unavailable", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		dispatcher
			.get("https://gitlab.com")
			.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/merge_requests/7/approvals" })
			.reply(404, { message: "Not available in this GitLab version" });
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_change",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "approve",
					id: 7,
					requestId: "approve-7",
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "unsupported_capability" });
	});

	it("rejects play_job when the concrete GitLab job is not manual", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/jobs?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/jobs/9" }).reply(200, {
			id: 9,
			status: "running",
		});
		const { service, context } = harness(true);

		await expect(
			service.execute(
				"forge_pipeline",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "play_job",
					id: 9,
					requestId: "play-9",
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "unsupported_capability" });
	});

	it("fails the merge gate cleanly when GitLab has no pipeline", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/merge_requests?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/merge_requests/7" }).reply(200, {
			id: 70,
			iid: 7,
			title: "Fixes #12",
			description: "Ready",
			target_branch: "main",
			labels: [],
		});
		api.intercept({
			method: "GET",
			path: "/api/v4/projects/acme%2Fwidget/merge_requests/7/pipelines?per_page=1",
		}).reply(200, []);
		const { service, context } = harness(true);
		mkdirSync(join(context.cwd, ".bone"), { recursive: true });
		writeFileSync(
			join(context.cwd, ".bone", "forge.yaml"),
			"version: 1\nprovider: gitlab\nworkflow:\n  requireSuccessfulPipeline: true\n",
		);

		await expect(
			service.execute(
				"forge_change",
				{
					remote: "https://gitlab.com/acme/widget.git",
					action: "merge",
					id: 7,
					requestId: "merge-without-pipeline",
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "policy_denied" });
	});

	it("continues a full GitHub review page even when the pagination header is missing", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls/7" }).reply(200, {
			id: 70,
			number: 7,
			title: "Fixes #12",
			body: "Ready",
			labels: [],
			base: { ref: "main" },
		});
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls/7/reviews?per_page=100&page=1" }).reply(200, [
			{ id: 1, state: "APPROVED", user: { login: "reviewer" } },
			...Array.from({ length: 99 }, (_, index) => ({
				id: index + 2,
				state: "COMMENTED",
				user: { login: `commenter-${index}` },
			})),
		]);
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls/7/reviews?per_page=100&page=2" }).reply(200, [
			{ id: 2, state: "DISMISSED", user: { login: "reviewer" } },
		]);
		const { service, context } = harness(true);
		mkdirSync(join(context.cwd, ".bone"), { recursive: true });
		writeFileSync(
			join(context.cwd, ".bone", "forge.yaml"),
			"version: 1\nprovider: github\nworkflow:\n  requiredApprovals: 1\n",
		);

		await expect(
			service.execute(
				"forge_change",
				{
					remote: "https://github.com/acme/widget.git",
					action: "merge",
					id: 7,
					requestId: "merge-paginated-reviews",
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "policy_denied" });
	});

	it("watches the latest GitHub review state across pages", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls/7/reviews?per_page=100&page=1" }).reply(200, [
			{ id: 1, state: "APPROVED", user: { login: "reviewer" } },
			...Array.from({ length: 99 }, (_, index) => ({
				id: index + 2,
				state: "COMMENTED",
				user: { login: `commenter-${index}` },
			})),
		]);
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls/7/reviews?per_page=100&page=2" }).reply(200, [
			{ id: 101, state: "DISMISSED", user: { login: "reviewer" } },
		]);
		const { service, context } = harness();

		await expect(
			service.execute(
				"forge_watch",
				{
					remote: "https://github.com/acme/widget.git",
					resource: "review",
					id: 7,
					until: ["pending"],
					timeoutSeconds: 1,
				},
				undefined,
				context,
			),
		).resolves.toMatchObject({ state: "pending" });
	});

	it("validates the concrete GitLab approvals response while watching reviews", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/merge_requests/7/approvals" }).reply(200, {
			approvals_left: 1,
		});
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/merge_requests/7/approvals" }).reply(200, {
			approved_by: [],
		});
		const { service, context } = harness();

		await expect(
			service.execute(
				"forge_watch",
				{
					remote: "https://gitlab.com/acme/widget.git",
					resource: "review",
					id: 7,
					until: ["approved"],
					timeoutSeconds: 1,
				},
				undefined,
				context,
			),
		).rejects.toMatchObject({ code: "invalid_remote_response" });
	});

	it("uses each GitHub reviewer's latest paginated state while watching", async () => {
		vi.stubEnv("GITHUB_TOKEN", "service-secret");
		const api = dispatcher.get("https://api.github.com");
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls?per_page=1" }).reply(200, []);
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls/7/reviews?per_page=100&page=1" }).reply(200, [
			{
				id: 1,
				state: "APPROVED",
				body: "REVIEW_BODY_SENTINEL",
				user: { login: "reviewer", avatar_url: "RAW_REVIEW_SENTINEL" },
			},
			...Array.from({ length: 99 }, (_, index) => ({
				id: index + 2,
				state: "COMMENTED",
				user: { login: `commenter-${index}` },
			})),
		]);
		api.intercept({ method: "GET", path: "/repos/acme/widget/pulls/7/reviews?per_page=100&page=2" }).reply(200, [
			{ id: 101, state: "DISMISSED", user: { login: "reviewer" } },
		]);
		const { service, context } = harness();

		const result = await service.execute(
			"forge_watch",
			{
				remote: "https://github.com/acme/widget.git",
				resource: "review",
				id: 7,
				until: ["pending"],
				timeoutSeconds: 1,
			},
			undefined,
			context,
		);

		expect(result).toMatchObject({
			state: "pending",
			reviewCount: 101,
			reviewerCount: 100,
			states: { dismissed: 1, commented: 99 },
		});
		expect(JSON.stringify(result)).not.toMatch(/REVIEW_BODY_SENTINEL|RAW_REVIEW_SENTINEL|avatar_url|reviews/);
	});
});
