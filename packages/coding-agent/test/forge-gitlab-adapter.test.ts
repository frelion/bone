import { MockAgent } from "undici";
import { afterEach, describe, expect, it } from "vitest";
import { parseGitRemote } from "../src/core/forge/context-resolver.ts";
import { ForgeError } from "../src/core/forge/errors.ts";
import { GitLabAdapter, parseGitLabVersion } from "../src/core/forge/gitlab-adapter.ts";
import { ForgeHttpClient } from "../src/core/forge/http-client.ts";
import { assertPublicNetworkHostname, isPrivateNetworkAddress } from "../src/core/forge/network-security.ts";

const agents: MockAgent[] = [];

function createAdapter(token = "glpat-secret-value"): { adapter: GitLabAdapter; mock: MockAgent } {
	const mock = new MockAgent();
	mock.disableNetConnect();
	agents.push(mock);
	return {
		mock,
		adapter: new GitLabAdapter({
			baseUrl: "http://gitlab.internal.example",
			token,
			allowedHosts: ["gitlab.internal.example"],
			dispatcher: mock,
			allowInsecureHttp: true,
		}),
	};
}

afterEach(async () => {
	await Promise.all(agents.splice(0).map((agent) => agent.close()));
});

describe("GitLab Forge adapter", () => {
	it("parses GitLab edition suffixes as semver", () => {
		expect(parseGitLabVersion("17.8.2-ee", "abc123")).toEqual({
			raw: "17.8.2-ee",
			semver: "17.8.2-ee",
			revision: "abc123",
			edition: "ee",
		});
		expect(() => parseGitLabVersion("not-a-version")).toThrow(ForgeError);
	});

	it("discovers a self-hosted GitLab version without rejecting unknown fields", async () => {
		const { adapter, mock } = createAdapter();
		mock
			.get("http://gitlab.internal.example")
			.intercept({ path: "/api/v4/version", method: "GET", headers: { "private-token": "glpat-secret-value" } })
			.reply(200, { version: "16.11.10-ce", revision: "42", future_field: true });

		await expect(adapter.getVersion()).resolves.toMatchObject({ semver: "16.11.10-ce", edition: "ce" });
	});

	it("preserves a self-managed API base path prefix", async () => {
		const mock = new MockAgent();
		mock.disableNetConnect();
		agents.push(mock);
		const adapter = new GitLabAdapter({
			baseUrl: "http://gitlab.internal.example/scm/api/v4",
			token: "secret",
			allowedHosts: ["gitlab.internal.example"],
			dispatcher: mock,
			allowInsecureHttp: true,
		});
		mock.get("http://gitlab.internal.example").intercept({ method: "GET", path: "/scm/api/v4/version" }).reply(200, {
			version: "17.8.2-ee",
		});

		await expect(adapter.getVersion()).resolves.toMatchObject({ semver: "17.8.2-ee" });
	});

	it("lists resources with encoded project paths and pagination", async () => {
		const { adapter, mock } = createAdapter();
		mock
			.get("http://gitlab.internal.example")
			.intercept({ path: "/api/v4/projects/group%2Fproject/issues?per_page=10&state=opened", method: "GET" })
			.reply(200, [{ id: 8, iid: 2, title: "Issue", extra: "preserved" }], { headers: { "x-next-page": "2" } });

		await expect(adapter.listIssues("group/project", { state: "opened" })).resolves.toEqual({
			items: [{ id: 8, iid: 2, title: "Issue", extra: "preserved" }],
			nextCursor: "2",
			hasMore: true,
		});
	});

	it("paginates Wiki pages, pipeline jobs, and releases with the same contract", async () => {
		const { adapter, mock } = createAdapter();
		const pool = mock.get("http://gitlab.internal.example");
		for (const [path, item] of [
			["/api/v4/projects/group%2Fproject/wikis?per_page=10&page=2", { slug: "runbook", title: "Runbook" }],
			["/api/v4/projects/group%2Fproject/pipelines/44/jobs?per_page=10&page=2", { id: 9, name: "test" }],
			["/api/v4/projects/group%2Fproject/releases?per_page=10&page=2", { tag_name: "v1.0.0" }],
		] as const) {
			pool.intercept({ path, method: "GET" }).reply(200, [item], { headers: { "x-next-page": "3" } });
		}

		const query = { per_page: 10, page: "2" };
		await expect(adapter.listWikiPages("group/project", query)).resolves.toMatchObject({
			nextCursor: "3",
			hasMore: true,
		});
		await expect(adapter.listPipelineJobs("group/project", 44, query)).resolves.toMatchObject({
			nextCursor: "3",
			hasMore: true,
		});
		await expect(adapter.listReleases("group/project", query)).resolves.toMatchObject({
			nextCursor: "3",
			hasMore: true,
		});
	});

	it("supports issue and Wiki writes with their different response shapes", async () => {
		const { adapter, mock } = createAdapter();
		const pool = mock.get("http://gitlab.internal.example");
		pool
			.intercept({ path: "/api/v4/projects/12/issues", method: "POST", body: JSON.stringify({ title: "One" }) })
			.reply(201, { id: 9, iid: 1, title: "One", server_extension: 1 });
		pool
			.intercept({
				path: "/api/v4/projects/12/wikis",
				method: "POST",
				body: JSON.stringify({ title: "Home", content: "Body" }),
			})
			.reply(201, { slug: "home", title: "Home", content: "Body" });

		await expect(adapter.createIssue(12, { title: "One" })).resolves.toMatchObject({ id: 9, server_extension: 1 });
		await expect(adapter.createWikiPage(12, { title: "Home", content: "Body" })).resolves.toMatchObject({
			slug: "home",
		});
	});

	it("maps capability probes without disabling the entire integration", async () => {
		const { adapter, mock } = createAdapter();
		const pool = mock.get("http://gitlab.internal.example");
		pool.intercept({ path: "/api/v4/projects/5/issues?per_page=1", method: "GET" }).reply(200, []);
		pool
			.intercept({ path: "/api/v4/projects/5/milestones?per_page=1", method: "GET" })
			.reply(403, { message: "Forbidden" });
		pool.intercept({ path: "/api/v4/projects/5/merge_requests?per_page=1", method: "GET" }).reply(200, []);
		pool
			.intercept({ path: "/api/v4/projects/5/wikis?per_page=1", method: "GET" })
			.reply(404, { message: "Wiki disabled" });
		pool
			.intercept({ path: "/api/v4/projects/5/pipelines?per_page=1", method: "GET" })
			.reply(404, { message: "CI disabled" });
		pool
			.intercept({ path: "/api/v4/projects/5/jobs?per_page=1", method: "GET" })
			.reply(405, { message: "Unavailable" });

		const capabilities = await adapter.getCapabilities(5);
		expect(Object.fromEntries(capabilities.map((item) => [item.id, item.state]))).toMatchObject({
			issue: "supported",
			milestone: "forbidden",
			merge_request: "supported",
			wiki: "disabled",
			pipeline: "disabled",
			job: "unsupported",
			"merge_request.approvals": "unknown",
			"pipeline.manual_job": "unknown",
		});
	});

	it("rejects redirects and never exposes the token in errors", async () => {
		const token = "glpat-must-not-leak";
		const { adapter, mock } = createAdapter(token);
		const pool = mock.get("http://gitlab.internal.example");
		pool
			.intercept({ path: "/api/v4/version", method: "GET" })
			.reply(302, "", { headers: { location: "https://attacker.example/steal" } });

		const redirectError = await adapter.getVersion().catch((error: unknown) => error);
		expect(redirectError).toBeInstanceOf(ForgeError);
		expect((redirectError as ForgeError).code).toBe("unsafe_remote");

		pool.intercept({ path: "/api/v4/projects/1/issues?per_page=10", method: "GET" }).reply(500, { message: token });
		const apiError = await adapter.listIssues(1).catch((error: unknown) => error);
		expect(String(apiError)).not.toContain(token);
		expect(String(apiError)).toContain("[REDACTED]");
	});

	it("bounds response bodies before parsing JSON", async () => {
		const mock = new MockAgent();
		mock.disableNetConnect();
		agents.push(mock);
		mock.get("http://gitlab.internal.example").intercept({ path: "/large", method: "GET" }).reply(200, {
			value: "response-is-too-large",
		});
		const client = new ForgeHttpClient({
			baseUrl: "http://gitlab.internal.example",
			provider: "gitlab",
			authHeaders: {},
			redactedSecrets: [],
			allowedHosts: ["gitlab.internal.example"],
			dispatcher: mock,
			allowInsecureHttp: true,
			maxResponseBytes: 8,
		});

		await expect(client.request("GET", "/large")).rejects.toMatchObject({ code: "invalid_remote_response" });
	});

	it("requires an allowlisted HTTPS instance outside tests", () => {
		expect(
			() =>
				new GitLabAdapter({
					baseUrl: "https://other.example",
					token: "secret",
					allowedHosts: ["gitlab.example"],
				}),
		).toThrow(/allowlisted/);
		expect(
			() =>
				new GitLabAdapter({
					baseUrl: "http://gitlab.example",
					token: "secret",
					allowedHosts: ["gitlab.example"],
				}),
		).toThrow(/HTTPS/);
	});
});

describe("Git remote context", () => {
	it("parses HTTPS and SCP-style remotes", () => {
		expect(parseGitRemote("https://gitlab.company.example/team/sub/project.git")).toMatchObject({
			provider: "gitlab",
			host: "gitlab.company.example",
			projectPath: "team/sub/project",
		});
		expect(parseGitRemote("git@github.com:owner/repo.git")).toMatchObject({
			provider: "github",
			host: "github.com",
			projectPath: "owner/repo",
		});
	});

	it("redacts credentials from HTTPS remotes", () => {
		const parsed = parseGitRemote("https://user:secret@gitlab.example/group/project.git");
		expect(parsed.remoteUrl).toBe("https://gitlab.example/group/project.git");
		expect(JSON.stringify(parsed)).not.toContain("secret");
	});

	it("classifies local, private, link-local, metadata, and IPv6 ULA destinations", () => {
		for (const address of [
			"127.0.0.1",
			"10.1.2.3",
			"172.16.4.5",
			"192.168.1.1",
			"169.254.169.254",
			"::1",
			"fd00::1",
			"fe80::1",
			"::ffff:127.0.0.1",
		]) {
			expect(isPrivateNetworkAddress(address), address).toBe(true);
		}
		expect(isPrivateNetworkAddress("8.8.8.8")).toBe(false);
		expect(isPrivateNetworkAddress("2606:4700:4700::1111")).toBe(false);
		expect(() => assertPublicNetworkHostname("127.0.0.1")).toThrow(/private network/);
		expect(() => assertPublicNetworkHostname("[::1]")).toThrow(/private network/);
		expect(() => assertPublicNetworkHostname("8.8.8.8")).not.toThrow();
	});
});
