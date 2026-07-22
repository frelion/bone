import type { TSchema } from "typebox";

export type AgentToolEffect = "read" | "routine_write" | "sensitive_write" | "destructive";
export type AgentToolIdempotency = "none" | "inherent" | "required";

export interface AgentToolRetryPolicy {
	retryableErrors: readonly string[];
	maxAttempts: number;
	rejectUnchangedRetry: boolean;
}

export interface AgentToolOutputBudget {
	defaultBytes: number;
	maximumBytes: number;
	maximumItems?: number;
}

export interface AgentToolExample {
	description: string;
	input: Readonly<Record<string, unknown>>;
}

export interface AgentToolContract {
	version: 1;
	useWhen: readonly string[];
	doNotUseWhen: readonly string[];
	outputSchema?: TSchema;
	effect: AgentToolEffect;
	idempotency: AgentToolIdempotency;
	retry: AgentToolRetryPolicy;
	outputBudget: AgentToolOutputBudget;
	examples: readonly AgentToolExample[];
}

export function defineAgentToolContract(contract: AgentToolContract): AgentToolContract {
	if (contract.outputBudget.defaultBytes <= 0) throw new Error("Agent tool default output budget must be positive");
	if (contract.outputBudget.maximumBytes < contract.outputBudget.defaultBytes) {
		throw new Error("Agent tool maximum output budget must not be smaller than its default budget");
	}
	if (!Number.isSafeInteger(contract.retry.maxAttempts) || contract.retry.maxAttempts < 0) {
		throw new Error("Agent tool retry maxAttempts must be a non-negative integer");
	}
	return Object.freeze(contract);
}
