import type { ForgePolicy } from "./policy.ts";

export type ForgeOperationRisk = "read" | "routine" | "sensitive" | "destructive";

export interface ForgeApprovalContext {
	interactive: boolean;
	confirmed?: boolean;
}

export interface ForgeApprovalDecision {
	allowed: boolean;
	requiresConfirmation: boolean;
	reason?: string;
}

export function decideForgeApproval(
	risk: ForgeOperationRisk,
	policy: ForgePolicy | undefined,
	context: ForgeApprovalContext,
): ForgeApprovalDecision {
	if (risk === "read") return { allowed: true, requiresConfirmation: false };
	if (!context.interactive) {
		return {
			allowed: false,
			requiresConfirmation: false,
			reason: "Forge writes require an interactive approval provider",
		};
	}
	const setting =
		risk === "routine"
			? (policy?.approvals.routineWrites ?? "auto")
			: risk === "sensitive"
				? (policy?.approvals.sensitiveWrites ?? "confirm")
				: (policy?.approvals.destructiveWrites ?? "confirm");
	if (setting === "deny")
		return { allowed: false, requiresConfirmation: false, reason: "Forge policy denies this write" };
	if (setting === "auto") return { allowed: true, requiresConfirmation: false };
	if (context.confirmed === true) return { allowed: true, requiresConfirmation: false };
	return { allowed: false, requiresConfirmation: true, reason: "Explicit confirmation is required" };
}

export function enforceForgeApproval(
	risk: ForgeOperationRisk,
	policy: ForgePolicy | undefined,
	context: ForgeApprovalContext,
): void {
	const decision = decideForgeApproval(risk, policy, context);
	if (!decision.allowed)
		throw new ForgeApprovalError(decision.requiresConfirmation, decision.reason ?? "Forge write denied");
}

export class ForgeApprovalError extends Error {
	readonly code: "approval_required" | "policy_denied";
	readonly requiresConfirmation: boolean;

	constructor(requiresConfirmation: boolean, message: string) {
		super(message);
		this.name = "ForgeApprovalError";
		this.requiresConfirmation = requiresConfirmation;
		this.code = requiresConfirmation ? "approval_required" : "policy_denied";
	}
}
