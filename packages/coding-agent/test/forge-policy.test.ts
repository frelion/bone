import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { decideForgeApproval } from "../src/core/forge/approval.ts";
import { loadForgeConfig, resolveForgeProvider, saveForgeConfig } from "../src/core/forge/config.ts";
import { ForgeCredentialStore } from "../src/core/forge/credential-store.ts";
import { ForgeMutationConflictError, ForgeMutationJournal } from "../src/core/forge/mutation-journal.ts";
import {
	enforceForgePolicy,
	evaluateForgePolicy,
	ForgePolicyDeniedError,
	loadForgePolicy,
	parseForgePolicy,
} from "../src/core/forge/policy.ts";

const directories: string[] = [];

function temporaryDirectory(): string {
	const directory = mkdtempSync(join(tmpdir(), "bone-forge-policy-"));
	directories.push(directory);
	return directory;
}

afterEach(() => {
	for (const directory of directories.splice(0)) {
		rmSync(directory, { recursive: true, force: true });
	}
});

describe("Forge configuration and credentials", () => {
	it("loads only explicitly configured HTTPS instances", () => {
		const agentDir = temporaryDirectory();
		writeFileSync(
			join(agentDir, "forge.json"),
			JSON.stringify({
				version: 1,
				instances: [
					{
						provider: "gitlab",
						host: "GitLab.Example.com",
						apiBaseUrl: "https://gitlab.example.com/api/v4/",
						credential: "gitlab:gitlab.example.com",
						allowPrivateNetwork: true,
					},
				],
			}),
		);
		expect(loadForgeConfig(agentDir).instances[0]).toMatchObject({
			host: "gitlab.example.com",
			apiBaseUrl: "https://gitlab.example.com/api/v4",
		});
	});

	it("rejects mismatched API hosts and unknown configuration", () => {
		const agentDir = temporaryDirectory();
		writeFileSync(
			join(agentDir, "forge.json"),
			JSON.stringify({
				version: 1,
				instances: [{ provider: "gitlab", host: "gitlab.example", apiBaseUrl: "https://evil.example/api/v4" }],
			}),
		);
		expect(() => loadForgeConfig(agentDir)).toThrow("must match");
	});

	it("allows only the fixed GitHub public REST host exception", () => {
		const agentDir = temporaryDirectory();
		writeFileSync(
			join(agentDir, "forge.json"),
			JSON.stringify({
				version: 1,
				instances: [{ provider: "github", host: "github.com", apiBaseUrl: "https://api.github.com" }],
			}),
		);
		expect(loadForgeConfig(agentDir).instances[0]?.apiBaseUrl).toBe("https://api.github.com");
	});

	it("atomically saves Forge instances with private file permissions", () => {
		const agentDir = temporaryDirectory();
		saveForgeConfig(agentDir, {
			instances: [
				{
					provider: "gitlab",
					host: "gitlab.example.com",
					apiBaseUrl: "https://gitlab.example.com",
					credential: "gitlab:gitlab.example.com",
					allowPrivateNetwork: false,
				},
			],
		});
		const path = join(agentDir, "forge.json");
		expect(loadForgeConfig(agentDir).instances).toHaveLength(1);
		if (process.platform !== "win32") expect(statSync(path).mode & 0o777).toBe(0o600);
	});

	it("uses the sole configured provider for a custom Forge host", () => {
		const config = {
			instances: [
				{
					provider: "github" as const,
					host: "github.company.test",
					apiBaseUrl: "https://github.company.test/api/v3",
					allowPrivateNetwork: true,
				},
			],
		};
		expect(resolveForgeProvider(config, "github.company.test", "gitlab")).toBe("github");
	});

	it("writes credentials as 0600 and resolves an environment reference without persisting its value", () => {
		const agentDir = temporaryDirectory();
		const store = new ForgeCredentialStore(agentDir);
		store.set("gitlab:example", { type: "token", token: "$BONE_TEST_GITLAB_TOKEN" });
		const authPath = join(agentDir, "forge-auth.json");
		expect(statSync(authPath).mode & 0o777).toBe(0o600);
		const resolved = store.resolve("gitlab:example", { BONE_TEST_GITLAB_TOKEN: "secret-value" });
		expect(resolved?.token).toBe("secret-value");
		expect(JSON.stringify(resolved)).not.toContain("secret-value");
		expect(readFileSync(authPath, "utf8")).not.toContain("secret-value");
	});

	it("removes a Forge credential without exposing other stored tokens", () => {
		const agentDir = temporaryDirectory();
		const store = new ForgeCredentialStore(agentDir);
		store.set("gitlab:first", { type: "token", token: "first-secret" });
		store.set("gitlab:second", { type: "token", token: "$SECOND_TOKEN" });
		expect(store.has("gitlab:first")).toBe(true);
		store.remove("gitlab:first");
		expect(store.has("gitlab:first")).toBe(false);
		expect(store.resolve("gitlab:second", { SECOND_TOKEN: "second-secret" })?.token).toBe("second-secret");
	});

	it.runIf(process.platform !== "win32")("refuses a credential file readable by other users", () => {
		const agentDir = temporaryDirectory();
		const authPath = join(agentDir, "forge-auth.json");
		writeFileSync(authPath, JSON.stringify({ key: { type: "token", token: "secret" } }));
		chmodSync(authPath, 0o644);
		expect(() => new ForgeCredentialStore(agentDir).resolve("key")).toThrow("permissions must be 0600");
	});
});

const POLICY = `
version: 1
provider: gitlab
workflow:
  issueRequired: true
  branchPattern: "^(feature|fix)/[0-9]+-[a-z0-9-]+$"
  requireCleanWorktreeForReview: true
  requiredLabels: [workflow::ready]
  requiredApprovals: 2
  blockUnresolvedDiscussions: true
  requireSuccessfulPipeline: true
  allowMergeMethods: [squash]
  protectedTargets: [main]
  release:
    requireMilestoneClosed: true
    requireTagFromProtectedBranch: true
approvals:
  routineWrites: auto
  sensitiveWrites: confirm
  destructiveWrites: confirm
  nonInteractiveWrites: deny
`;

describe("Forge policy", () => {
	it("strictly parses version 1 and rejects unknown keys", () => {
		const policy = parseForgePolicy(POLICY);
		expect(policy.workflow.requiredApprovals).toBe(2);
		expect(() => parseForgePolicy(`${POLICY}\nunknown: true\n`)).toThrow("unknown key unknown");
		expect(() => parseForgePolicy("version: 2\n")).toThrow("expected 1");
	});

	it("does not load project policy before trust is granted", () => {
		const cwd = temporaryDirectory();
		const configDir = join(cwd, ".bone");
		writeFileSync(join(cwd, "placeholder"), "");
		mkdirSync(configDir);
		writeFileSync(join(configDir, "forge.yaml"), POLICY);
		expect(() => loadForgePolicy(cwd, false)).toThrow("not trusted");
	});

	it("applies only the gates relevant to each workflow stage", () => {
		const policy = parseForgePolicy(POLICY);
		expect(evaluateForgePolicy(policy, { issueLinked: true, branch: "feature/12-add-api" }, "start_issue")).toEqual(
			[],
		);
		const reviewCodes = evaluateForgePolicy(
			policy,
			{ issueLinked: true, branch: "feature/12-add-api", worktreeClean: false, labels: [] },
			"submit_review",
		).map((violation) => violation.code);
		expect(reviewCodes).toEqual(["dirty_worktree", "required_labels"]);
		expect(reviewCodes).not.toContain("required_approvals");
	});

	it("hard-denies protected branch merges but does not apply protected gates to other targets", () => {
		const policy = parseForgePolicy(POLICY);
		const facts = { issueLinked: true, targetBranch: "main", mergeMethod: "merge" as const };
		const codes = evaluateForgePolicy(policy, facts, "merge").map((violation) => violation.code);
		expect(codes).toEqual([
			"required_labels",
			"required_approvals",
			"unresolved_discussions",
			"pipeline_required",
			"merge_method",
		]);
		expect(() => enforceForgePolicy(policy, facts, "merge")).toThrow(ForgePolicyDeniedError);
		expect(evaluateForgePolicy(policy, { ...facts, targetBranch: "develop" }, "merge")).toEqual([]);
	});

	it("fails closed when a protected target or restricted merge method is unknown", () => {
		const policy = parseForgePolicy(POLICY);
		const codes = evaluateForgePolicy(
			policy,
			{
				issueLinked: true,
				labels: ["workflow::ready"],
				approvals: 2,
				unresolvedDiscussions: 0,
				pipelineSuccessful: true,
			},
			"merge",
		).map((violation) => violation.code);
		expect(codes).toEqual(["protected_target_unknown", "merge_method"]);
	});

	it("applies release-only gates only during release", () => {
		const policy = parseForgePolicy(POLICY);
		expect(evaluateForgePolicy(policy, { pipelineSuccessful: true }, "release").map((item) => item.code)).toEqual([
			"milestone_closed",
			"tag_from_protected_branch",
		]);
		expect(
			evaluateForgePolicy(
				policy,
				{ pipelineSuccessful: true, milestoneClosed: true, tagFromProtectedBranch: true },
				"release",
			),
		).toEqual([]);
	});
});

describe("Forge approvals", () => {
	const policy = parseForgePolicy(POLICY);

	it("allows reads but fails closed for every non-interactive write", () => {
		expect(decideForgeApproval("read", policy, { interactive: false }).allowed).toBe(true);
		expect(decideForgeApproval("routine", policy, { interactive: false })).toMatchObject({
			allowed: false,
			requiresConfirmation: false,
		});
	});

	it("fails closed for non-interactive sensitive and destructive writes", () => {
		for (const risk of ["sensitive", "destructive"] as const) {
			expect(decideForgeApproval(risk, policy, { interactive: false })).toMatchObject({
				allowed: false,
				requiresConfirmation: false,
			});
		}
	});

	it("requires and accepts an explicit interactive confirmation", () => {
		expect(decideForgeApproval("destructive", policy, { interactive: true })).toMatchObject({
			allowed: false,
			requiresConfirmation: true,
		});
		expect(decideForgeApproval("destructive", policy, { interactive: true, confirmed: true }).allowed).toBe(true);
	});
});

describe("Forge mutation journal", () => {
	it("replays completed requests and rejects requestId reuse", () => {
		const journal = new ForgeMutationJournal(temporaryDirectory());
		expect(journal.begin("request-1", "fingerprint-a").action).toBe("execute");
		journal.complete("request-1", "fingerprint-a", { id: 42 });
		expect(journal.begin("request-1", "fingerprint-a")).toMatchObject({ action: "replay", result: { id: 42 } });
		expect(() => journal.begin("request-1", "fingerprint-b")).toThrow(ForgeMutationConflictError);
	});

	it("preserves ambiguous outcomes and never retries them blindly", () => {
		const journal = new ForgeMutationJournal(temporaryDirectory());
		journal.begin("request-2", "fingerprint-a");
		journal.markAmbiguous("request-2", "fingerprint-a", "connection closed after upload");
		expect(journal.begin("request-2", "fingerprint-a")).toMatchObject({
			action: "ambiguous",
			entry: { status: "ambiguous", error: "connection closed after upload" },
		});
	});

	it("allows an explicitly failed operation to be retried with the same fingerprint", () => {
		const journal = new ForgeMutationJournal(temporaryDirectory());
		journal.begin("request-3", "fingerprint-a");
		journal.fail("request-3", "fingerprint-a", "validation rejected before mutation");
		expect(journal.begin("request-3", "fingerprint-a").action).toBe("execute");
	});
});
