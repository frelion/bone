import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { parseDocument } from "yaml";
import type { ForgeProvider } from "./contracts.ts";

export interface ForgeWorkflowPolicy {
	issueRequired: boolean;
	branchPattern?: string;
	requireCleanWorktreeForReview: boolean;
	requireIssueReference: boolean;
	requiredLabels: readonly string[];
	requiredApprovals: number;
	blockUnresolvedDiscussions: boolean;
	requireSuccessfulPipeline: boolean;
	allowMergeMethods: readonly ("merge" | "squash" | "rebase")[];
	protectedTargets: readonly string[];
	release: {
		requireMilestoneClosed: boolean;
		requireTagFromProtectedBranch: boolean;
	};
}

export type ForgeApprovalSetting = "auto" | "confirm" | "deny";

export interface ForgePolicy {
	version: 1;
	provider?: ForgeProvider;
	workflow: ForgeWorkflowPolicy;
	approvals: {
		routineWrites: ForgeApprovalSetting;
		sensitiveWrites: ForgeApprovalSetting;
		destructiveWrites: ForgeApprovalSetting;
		nonInteractiveWrites: "deny";
	};
}

export interface ForgePolicyFacts {
	issueLinked?: boolean;
	branch?: string;
	worktreeClean?: boolean;
	labels?: readonly string[];
	approvals?: number;
	unresolvedDiscussions?: number;
	pipelineSuccessful?: boolean;
	mergeMethod?: "merge" | "squash" | "rebase";
	targetBranch?: string;
	milestoneClosed?: boolean;
	tagFromProtectedBranch?: boolean;
}

export interface ForgePolicyViolation {
	code: string;
	message: string;
	actual?: unknown;
	expected?: unknown;
}

export type ForgePolicyStage = "start_issue" | "submit_review" | "merge" | "release";

const DEFAULT_WORKFLOW: ForgeWorkflowPolicy = {
	issueRequired: false,
	requireCleanWorktreeForReview: false,
	requireIssueReference: false,
	requiredLabels: [],
	requiredApprovals: 0,
	blockUnresolvedDiscussions: false,
	requireSuccessfulPipeline: false,
	allowMergeMethods: ["merge", "squash", "rebase"],
	protectedTargets: [],
	release: { requireMilestoneClosed: false, requireTagFromProtectedBranch: false },
};

const DEFAULT_APPROVALS: ForgePolicy["approvals"] = {
	routineWrites: "auto",
	sensitiveWrites: "confirm",
	destructiveWrites: "confirm",
	nonInteractiveWrites: "deny",
};

function record(value: unknown, path: string): Record<string, unknown> {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		throw new Error(`Invalid Forge policy at ${path}: expected an object`);
	}
	return value as Record<string, unknown>;
}

function keys(value: Record<string, unknown>, allowed: readonly string[], path: string): void {
	for (const key of Object.keys(value)) {
		if (!allowed.includes(key)) throw new Error(`Invalid Forge policy at ${path}: unknown key ${key}`);
	}
}

function boolean(value: unknown, path: string, fallback: boolean): boolean {
	if (value === undefined) return fallback;
	if (typeof value !== "boolean") throw new Error(`Invalid Forge policy at ${path}: expected a boolean`);
	return value;
}

function strings(value: unknown, path: string, fallback: readonly string[]): string[] {
	if (value === undefined) return [...fallback];
	if (!Array.isArray(value) || value.some((item) => typeof item !== "string" || item.length === 0)) {
		throw new Error(`Invalid Forge policy at ${path}: expected non-empty strings`);
	}
	return [...new Set(value as string[])];
}

export function parseForgePolicy(source: string): ForgePolicy {
	const document = parseDocument(source, { prettyErrors: true, strict: true, uniqueKeys: true });
	if (document.errors.length > 0) throw new Error(`Invalid Forge policy YAML: ${document.errors[0].message}`);
	const root = record(document.toJS({ maxAliasCount: 20 }), "root");
	keys(root, ["version", "provider", "workflow", "approvals"], "root");
	if (root.version !== 1) throw new Error("Invalid Forge policy at version: expected 1");
	if (root.provider !== undefined && root.provider !== "gitlab" && root.provider !== "github") {
		throw new Error("Invalid Forge policy at provider");
	}
	const workflow = root.workflow === undefined ? {} : record(root.workflow, "workflow");
	keys(
		workflow,
		[
			"issueRequired",
			"branchPattern",
			"requireCleanWorktreeForReview",
			"requireIssueReference",
			"requiredLabels",
			"requiredApprovals",
			"blockUnresolvedDiscussions",
			"requireSuccessfulPipeline",
			"allowMergeMethods",
			"protectedTargets",
			"release",
		],
		"workflow",
	);
	if (workflow.branchPattern !== undefined && typeof workflow.branchPattern !== "string") {
		throw new Error("Invalid Forge policy at workflow.branchPattern");
	}
	if (typeof workflow.branchPattern === "string") {
		try {
			new RegExp(workflow.branchPattern, "u");
		} catch {
			throw new Error("Invalid Forge policy at workflow.branchPattern: invalid regular expression");
		}
	}
	if (
		workflow.requiredApprovals !== undefined &&
		(!Number.isSafeInteger(workflow.requiredApprovals) || (workflow.requiredApprovals as number) < 0)
	) {
		throw new Error("Invalid Forge policy at workflow.requiredApprovals: expected a non-negative integer");
	}
	const mergeMethods = strings(
		workflow.allowMergeMethods,
		"workflow.allowMergeMethods",
		DEFAULT_WORKFLOW.allowMergeMethods,
	);
	if (mergeMethods.some((method) => method !== "merge" && method !== "squash" && method !== "rebase")) {
		throw new Error("Invalid Forge policy at workflow.allowMergeMethods");
	}
	const release = workflow.release === undefined ? {} : record(workflow.release, "workflow.release");
	keys(release, ["requireMilestoneClosed", "requireTagFromProtectedBranch"], "workflow.release");
	const approvals = root.approvals === undefined ? {} : record(root.approvals, "approvals");
	keys(approvals, ["routineWrites", "sensitiveWrites", "destructiveWrites", "nonInteractiveWrites"], "approvals");
	function approval(name: keyof typeof DEFAULT_APPROVALS): ForgeApprovalSetting {
		const value = approvals[name] ?? DEFAULT_APPROVALS[name];
		if (value !== "auto" && value !== "confirm" && value !== "deny") {
			throw new Error(`Invalid Forge policy at approvals.${name}`);
		}
		return value;
	}
	if (approvals.nonInteractiveWrites !== undefined && approvals.nonInteractiveWrites !== "deny") {
		throw new Error("Invalid Forge policy at approvals.nonInteractiveWrites: only deny is supported");
	}
	return {
		version: 1,
		provider: root.provider as ForgeProvider | undefined,
		workflow: {
			issueRequired: boolean(workflow.issueRequired, "workflow.issueRequired", DEFAULT_WORKFLOW.issueRequired),
			branchPattern: workflow.branchPattern as string | undefined,
			requireCleanWorktreeForReview: boolean(
				workflow.requireCleanWorktreeForReview,
				"workflow.requireCleanWorktreeForReview",
				DEFAULT_WORKFLOW.requireCleanWorktreeForReview,
			),
			requireIssueReference: boolean(
				workflow.requireIssueReference,
				"workflow.requireIssueReference",
				DEFAULT_WORKFLOW.requireIssueReference,
			),
			requiredLabels: strings(workflow.requiredLabels, "workflow.requiredLabels", []),
			requiredApprovals: (workflow.requiredApprovals as number | undefined) ?? 0,
			blockUnresolvedDiscussions: boolean(
				workflow.blockUnresolvedDiscussions,
				"workflow.blockUnresolvedDiscussions",
				DEFAULT_WORKFLOW.blockUnresolvedDiscussions,
			),
			requireSuccessfulPipeline: boolean(
				workflow.requireSuccessfulPipeline,
				"workflow.requireSuccessfulPipeline",
				DEFAULT_WORKFLOW.requireSuccessfulPipeline,
			),
			allowMergeMethods: mergeMethods as ForgeWorkflowPolicy["allowMergeMethods"],
			protectedTargets: strings(workflow.protectedTargets, "workflow.protectedTargets", []),
			release: {
				requireMilestoneClosed: boolean(
					release.requireMilestoneClosed,
					"workflow.release.requireMilestoneClosed",
					false,
				),
				requireTagFromProtectedBranch: boolean(
					release.requireTagFromProtectedBranch,
					"workflow.release.requireTagFromProtectedBranch",
					false,
				),
			},
		},
		approvals: {
			routineWrites: approval("routineWrites"),
			sensitiveWrites: approval("sensitiveWrites"),
			destructiveWrites: approval("destructiveWrites"),
			nonInteractiveWrites: "deny",
		},
	};
}

export function loadForgePolicy(cwd: string, projectTrusted: boolean): ForgePolicy | undefined {
	const path = join(cwd, ".bone", "forge.yaml");
	if (!existsSync(path)) return undefined;
	if (!projectTrusted) throw new Error("Project is not trusted; refusing to load Forge policy");
	return parseForgePolicy(readFileSync(path, "utf8"));
}

export function evaluateForgePolicy(
	policy: ForgePolicy,
	facts: ForgePolicyFacts,
	stage: ForgePolicyStage = "merge",
): ForgePolicyViolation[] {
	const violations: ForgePolicyViolation[] = [];
	const workflow = policy.workflow;
	if (
		stage !== "release" &&
		(workflow.issueRequired || workflow.requireIssueReference) &&
		facts.issueLinked !== true
	) {
		violations.push({
			code: "issue_required",
			message: "A linked issue is required",
			actual: facts.issueLinked,
			expected: true,
		});
	}
	if (
		(stage === "start_issue" || stage === "submit_review") &&
		workflow.branchPattern &&
		(facts.branch === undefined || !new RegExp(workflow.branchPattern, "u").test(facts.branch))
	) {
		violations.push({
			code: "branch_pattern",
			message: "Branch name does not match policy",
			actual: facts.branch,
			expected: workflow.branchPattern,
		});
	}
	if (stage === "submit_review" && workflow.requireCleanWorktreeForReview && facts.worktreeClean !== true) {
		violations.push({
			code: "dirty_worktree",
			message: "A clean worktree is required",
			actual: facts.worktreeClean,
			expected: true,
		});
	}
	const protectedTargetUnknown =
		stage === "merge" && workflow.protectedTargets.length > 0 && facts.targetBranch === undefined;
	if (protectedTargetUnknown) {
		violations.push({
			code: "protected_target_unknown",
			message: "The target branch could not be verified",
			actual: facts.targetBranch,
			expected: workflow.protectedTargets,
		});
	}
	const protectedMerge =
		stage === "merge" &&
		(workflow.protectedTargets.length === 0 ||
			facts.targetBranch === undefined ||
			workflow.protectedTargets.includes(facts.targetBranch));
	if (stage === "submit_review" || protectedMerge) {
		const labels = new Set(facts.labels ?? []);
		const missingLabels = workflow.requiredLabels.filter((label) => !labels.has(label));
		if (missingLabels.length > 0) {
			violations.push({
				code: "required_labels",
				message: "Required labels are missing",
				actual: [...labels],
				expected: workflow.requiredLabels,
			});
		}
	}
	if (protectedMerge && workflow.requiredApprovals > 0 && (facts.approvals ?? 0) < workflow.requiredApprovals) {
		violations.push({
			code: "required_approvals",
			message: "Not enough approvals",
			actual: facts.approvals ?? 0,
			expected: workflow.requiredApprovals,
		});
	}
	if (
		protectedMerge &&
		workflow.blockUnresolvedDiscussions &&
		(facts.unresolvedDiscussions === undefined || facts.unresolvedDiscussions > 0)
	) {
		violations.push({
			code: "unresolved_discussions",
			message: "All discussions must be resolved",
			actual: facts.unresolvedDiscussions,
			expected: 0,
		});
	}
	if (
		(protectedMerge || stage === "release") &&
		workflow.requireSuccessfulPipeline &&
		facts.pipelineSuccessful !== true
	) {
		violations.push({
			code: "pipeline_required",
			message: "A successful pipeline is required",
			actual: facts.pipelineSuccessful,
			expected: true,
		});
	}
	if (
		protectedMerge &&
		(facts.mergeMethod === undefined
			? workflow.allowMergeMethods.length < DEFAULT_WORKFLOW.allowMergeMethods.length
			: !workflow.allowMergeMethods.includes(facts.mergeMethod))
	) {
		violations.push({
			code: "merge_method",
			message: "Merge method is not allowed",
			actual: facts.mergeMethod,
			expected: workflow.allowMergeMethods,
		});
	}
	if (stage === "release" && workflow.release.requireMilestoneClosed && facts.milestoneClosed !== true) {
		violations.push({
			code: "milestone_closed",
			message: "The release milestone must be closed",
			actual: facts.milestoneClosed,
			expected: true,
		});
	}
	if (stage === "release" && workflow.release.requireTagFromProtectedBranch && facts.tagFromProtectedBranch !== true) {
		violations.push({
			code: "tag_from_protected_branch",
			message: "The release tag must originate from a protected branch",
			actual: facts.tagFromProtectedBranch,
			expected: true,
		});
	}
	return violations;
}

export function enforceForgePolicy(
	policy: ForgePolicy,
	facts: ForgePolicyFacts,
	stage: ForgePolicyStage = "merge",
): void {
	const violations = evaluateForgePolicy(policy, facts, stage);
	if (violations.length > 0) throw new ForgePolicyDeniedError(violations);
}

export class ForgePolicyDeniedError extends Error {
	readonly code = "policy_denied";
	readonly violations: readonly ForgePolicyViolation[];

	constructor(violations: readonly ForgePolicyViolation[]) {
		super(`Forge policy denied the operation: ${violations.map((violation) => violation.code).join(", ")}`);
		this.name = "ForgePolicyDeniedError";
		this.violations = violations;
	}
}
