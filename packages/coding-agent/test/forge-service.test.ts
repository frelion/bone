import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { getGlobalDispatcher, MockAgent, setGlobalDispatcher } from "undici";
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
		api.intercept({ method: "GET", path: "/user" }).reply(200, { id: 9, login: "octo" });
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
			user: { id: 9, login: "octo" },
		});
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
		).resolves.toMatchObject({ slug: "operations/runbook" });
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
		).resolves.toMatchObject({ tag_name: "v1.2.0" });
	});

	it("journals mutating workflow transitions", async () => {
		vi.stubEnv("GITLAB_TOKEN", "service-secret");
		const api = dispatcher.get("https://gitlab.com");
		api.intercept({ method: "GET", path: "/api/v4/projects/acme%2Fwidget/releases?per_page=1" }).reply(200, []);
		api.intercept({ method: "POST", path: "/api/v4/projects/acme%2Fwidget/releases" }).reply(201, {
			tag_name: "v2.0.0",
			name: "Version 2",
		});
		const { service, context } = harness(true);
		const input = {
			remote: "https://gitlab.com/acme/widget.git",
			transition: "release",
			requestId: "transition-release",
			input: { tag_name: "v2.0.0", name: "Version 2" },
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
		).resolves.toMatchObject({ state: "success", data: { id: 4 } });
		await expect(
			service.execute("forge_watch", { ...common, resource: "job", id: 9, until: ["failed"] }, undefined, context),
		).resolves.toMatchObject({ state: "failed", data: { id: 9 } });
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
});
