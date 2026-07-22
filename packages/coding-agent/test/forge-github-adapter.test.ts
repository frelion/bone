import { MockAgent } from "undici";
import { afterEach, describe, expect, it } from "vitest";
import { GitHubAdapter } from "../src/core/forge/github-adapter.ts";

const agents: MockAgent[] = [];

afterEach(async () => {
	await Promise.all(agents.splice(0).map((agent) => agent.close()));
});

function adapter(): { adapter: GitHubAdapter; mock: ReturnType<MockAgent["get"]> } {
	const dispatcher = new MockAgent();
	dispatcher.disableNetConnect();
	agents.push(dispatcher);
	return {
		adapter: new GitHubAdapter({
			baseUrl: "https://api.github.com",
			token: "github-secret",
			allowedHosts: ["api.github.com"],
			dispatcher,
		}),
		mock: dispatcher.get("https://api.github.com"),
	};
}

describe("GitHubAdapter", () => {
	it("normalizes Link pagination and sends versioned bearer authentication", async () => {
		const { adapter: github, mock } = adapter();
		mock
			.intercept({
				method: "GET",
				path: "/repos/acme/widget/issues?per_page=2&state=open",
				headers: {
					authorization: "Bearer github-secret",
					"x-github-api-version": "2022-11-28",
					"user-agent": "bone-forge",
				},
			})
			.reply(200, [{ id: 11, number: 3, title: "Example" }], {
				headers: { link: '<https://api.github.com/repositories/1/issues?page=2>; rel="next"' },
			});

		const result = await github.listIssues("acme/widget", { per_page: 2, state: "open" });

		expect(result.items).toEqual([expect.objectContaining({ id: 11, number: 3 })]);
		expect(result).toMatchObject({ nextCursor: "2", hasMore: true });
	});

	it("unwraps workflow jobs while preserving cursor pagination", async () => {
		const { adapter: github, mock } = adapter();
		mock.intercept({ method: "GET", path: "/repos/acme/widget/actions/runs/44/jobs?per_page=10&page=2" }).reply(
			200,
			{ total_count: 12, jobs: [{ id: 9, name: "test", status: "completed" }] },
			{
				headers: { link: '<https://api.github.com/repos/acme/widget/actions/runs/44/jobs?page=3>; rel="next"' },
			},
		);

		await expect(github.listWorkflowJobs("acme/widget", 44, { per_page: 10, page: "2" })).resolves.toMatchObject({
			items: [{ id: 9, name: "test", status: "completed" }],
			nextCursor: "3",
			hasMore: true,
		});
	});

	it("uses repository-scoped GitHub Search for issue text", async () => {
		const { adapter: github, mock } = adapter();
		mock
			.intercept({
				method: "GET",
				path: "/search/issues?per_page=10&page=2&q=database+deadlock+repo%3Aacme%2Fwidget+is%3Aissue+is%3Aopen",
			})
			.reply(200, { items: [{ id: 11, number: 3, title: "Deadlock" }] });

		await expect(
			github.searchIssues("acme/widget", "database deadlock", "issue", {
				per_page: 10,
				page: "2",
				state: "open",
			}),
		).resolves.toMatchObject({ items: [{ id: 11, number: 3 }] });
	});

	it("reports GitHub Wiki as unsupported while preserving other capability probes", async () => {
		const { adapter: github, mock } = adapter();
		for (const path of [
			"/repos/acme/widget/issues?per_page=1",
			"/repos/acme/widget/milestones?per_page=1",
			"/repos/acme/widget/pulls?per_page=1",
			"/repos/acme/widget/actions/runs?per_page=1",
			"/repos/acme/widget/releases?per_page=1",
		]) {
			mock.intercept({ method: "GET", path }).reply(200, path.includes("actions/runs") ? { workflow_runs: [] } : []);
		}

		const capabilities = await github.getCapabilities("acme/widget");

		expect(capabilities).toContainEqual(expect.objectContaining({ id: "issue", state: "supported" }));
		expect(capabilities).toContainEqual(expect.objectContaining({ id: "wiki", state: "unsupported" }));
	});

	it.each([
		["empty", ""],
		["non-JSON", "not json"],
		["JSON", []],
	])("treats a successful release %s response as a supported capability", async (_label, releaseBody) => {
		const { adapter: github, mock } = adapter();
		for (const path of [
			"/repos/acme/widget/issues?per_page=1",
			"/repos/acme/widget/milestones?per_page=1",
			"/repos/acme/widget/pulls?per_page=1",
			"/repos/acme/widget/actions/runs?per_page=1",
		]) {
			mock.intercept({ method: "GET", path }).reply(200, []);
		}
		mock.intercept({ method: "GET", path: "/repos/acme/widget/releases?per_page=1" }).reply(200, releaseBody);

		await expect(github.getCapabilities("acme/widget")).resolves.toContainEqual({
			id: "release",
			state: "supported",
		});
	});

	it.each([
		[401, "forbidden"],
		[403, "forbidden"],
		[404, "disabled"],
		[500, "unknown"],
	] as const)("maps release probe status %i to %s without leaking credentials", async (status, state) => {
		const { adapter: github, mock } = adapter();
		for (const path of [
			"/repos/acme/widget/issues?per_page=1",
			"/repos/acme/widget/milestones?per_page=1",
			"/repos/acme/widget/pulls?per_page=1",
			"/repos/acme/widget/actions/runs?per_page=1",
		]) {
			mock.intercept({ method: "GET", path }).reply(200, []);
		}
		mock
			.intercept({ method: "GET", path: "/repos/acme/widget/releases?per_page=1" })
			.reply(status, { message: "github-secret probe failure" });

		const release = (await github.getCapabilities("acme/widget")).find((capability) => capability.id === "release");

		expect(release).toMatchObject({ id: "release", state });
		expect(release?.reason).not.toContain("github-secret");
		expect(release?.reason).toContain("[REDACTED]");
	});

	it("maps a release probe network failure to unknown without leaking credentials", async () => {
		const { adapter: github, mock } = adapter();
		for (const path of [
			"/repos/acme/widget/issues?per_page=1",
			"/repos/acme/widget/milestones?per_page=1",
			"/repos/acme/widget/pulls?per_page=1",
			"/repos/acme/widget/actions/runs?per_page=1",
		]) {
			mock.intercept({ method: "GET", path }).reply(200, []);
		}
		mock
			.intercept({ method: "GET", path: "/repos/acme/widget/releases?per_page=1" })
			.replyWithError(new Error("github-secret socket failure"));

		const release = (await github.getCapabilities("acme/widget")).find((capability) => capability.id === "release");

		expect(release).toMatchObject({ id: "release", state: "unknown" });
		expect(release?.reason).not.toContain("github-secret");
		expect(release?.reason).toContain("[REDACTED]");
	});

	it.each([
		["create", "POST", "/repos/acme/widget/releases"],
		["update", "PATCH", "/repos/acme/widget/releases/7"],
	] as const)("rejects an empty successful release %s response", async (action, method, path) => {
		const { adapter: github, mock } = adapter();
		mock.intercept({ method, path }).reply(action === "create" ? 201 : 200, "");

		const operation =
			action === "create"
				? github.createRelease("acme/widget", { tag_name: "v1.0.0" })
				: github.updateRelease("acme/widget", 7, { name: "Version 1" });

		await expect(operation).rejects.toMatchObject({ code: "invalid_remote_response" });
	});

	it("accepts GitHub's non-resource pull request merge response", async () => {
		const { adapter: github, mock } = adapter();
		mock.intercept({ method: "PUT", path: "/repos/acme/widget/pulls/7/merge" }).reply(200, {
			sha: "abc123",
			merged: true,
			message: "Pull Request successfully merged",
		});

		await expect(github.mergePullRequest("acme/widget", 7, { merge_method: "squash" })).resolves.toMatchObject({
			merged: true,
			sha: "abc123",
		});
	});
});
