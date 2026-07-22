import type { ForgeErrorCode } from "../../src/core/forge/errors.ts";

export type EvalCategory = "query" | "recovery" | "mutation" | "budget" | "execution_mode";

export interface EvalToolCall {
	name: string;
	args: Record<string, unknown>;
	id?: string;
}

export interface EvalAssistantStep {
	toolCalls?: EvalToolCall[];
	text?: string;
}

export interface EvalServiceStep {
	toolName: string;
	input: Record<string, unknown>;
	outcome:
		| { kind: "return"; value: unknown }
		| { kind: "throw"; code: ForgeErrorCode; message: string; details?: Record<string, unknown> };
	delayMs?: number;
}

export interface EvalExpectations {
	mustComplete: boolean;
	expectedServiceCalls: number;
	expectedServiceTools?: string[];
	expectedErrors?: string[];
	expectedDuplicateFailures?: number;
	expectedContextIncludesToolResult?: boolean;
	maxContextBytes?: number;
	maxToolResultBytes?: number;
	mustNotContain?: string[];
	assertions: string[];
}

export interface ForgeEvalCase {
	id: string;
	category: EvalCategory;
	prompt: string;
	steps: EvalAssistantStep[];
	service: EvalServiceStep[];
	expectations: EvalExpectations;
}

export interface EvalTrace {
	modelSteps: number;
	modelContexts: Array<{ step: number; messageCount: number; bytes: number; hasToolResult: boolean }>;
	serviceCalls: Array<{ toolName: string; input: Record<string, unknown>; startedAt: number; finishedAt: number }>;
	unexpectedServiceCalls: Array<{ toolName: string; input: Record<string, unknown>; reason: string }>;
	toolResults: Array<{ toolName: string; isError: boolean; text: string; bytes: number }>;
	finalAssistantText: string;
	completed: boolean;
	scriptExhausted: boolean;
}

export interface ForgeEvalCaseResult {
	id: string;
	category: EvalCategory;
	passed: boolean;
	assertions: Array<{ name: string; passed: boolean; detail?: string }>;
	trace: EvalTrace;
}

export interface ForgeEvalReport {
	schemaVersion: 1;
	mode: "scripted";
	generatedAt: string;
	summary: { total: number; passed: number; failed: number; protocolPassRate: number };
	cases: ForgeEvalCaseResult[];
}

export interface LiveForgeEvalCase {
	id: string;
	category: EvalCategory;
	prompt: string;
	service: EvalServiceStep[];
	expectedServiceTools: string[];
	mustNotContain?: string[];
}

export interface LiveForgeEvalTrace extends EvalTrace {
	run: number;
	model: string;
	toolCalls: Array<{ name: string; args: unknown }>;
	timedOut: boolean;
	turnLimitReached: boolean;
}

export interface LiveForgeEvalCaseResult {
	id: string;
	category: EvalCategory;
	run: number;
	passed: boolean;
	metrics: {
		firstToolSelectionCorrect: boolean;
		firstCallSchemaValid: boolean;
		correctionSucceeded: boolean | null;
		duplicateFailureCount: number;
		toolCallCount: number;
		contextBytes: number;
	};
	assertions: Array<{ name: string; passed: boolean; detail?: string }>;
	trace: LiveForgeEvalTrace;
}

export interface LiveForgeEvalReport {
	schemaVersion: 1;
	mode: "live";
	generatedAt: string;
	model: { provider: string; id: string };
	configuration: { repetitions: number; maxTurns: number; timeoutMs: number };
	summary: {
		total: number;
		passed: number;
		failed: number;
		taskCompletionRate: number;
		firstToolSelectionRate: number;
		firstCallValidRate: number;
		correctionSuccessRate: number | null;
		deterministicRepeatRate: number | null;
		meanToolCalls: number;
		meanContextBytes: number;
	};
	cases: LiveForgeEvalCaseResult[];
}

export type AnyForgeEvalReport = ForgeEvalReport | LiveForgeEvalReport;
