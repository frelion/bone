import { ForgeError } from "../../src/core/forge/errors.ts";
import type { ForgeService, ForgeToolContext } from "../../src/core/forge/tools.ts";
import type { EvalServiceStep } from "./types.ts";

function stableJson(value: unknown): string {
	if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
	if (typeof value !== "object" || value === null) return JSON.stringify(value);
	return `{${Object.entries(value as Record<string, unknown>)
		.sort(([left], [right]) => left.localeCompare(right))
		.map(([key, entry]) => `${JSON.stringify(key)}:${stableJson(entry)}`)
		.join(",")}}`;
}

export class FakeForgeService implements ForgeService {
	private readonly steps: readonly EvalServiceStep[];
	private nextStep = 0;
	readonly calls: Array<{ toolName: string; input: Record<string, unknown>; startedAt: number; finishedAt: number }> = [];
	readonly unexpectedCalls: Array<{ toolName: string; input: Record<string, unknown>; reason: string }> = [];

	constructor(
		steps: readonly EvalServiceStep[],
		private readonly options: {
			ignoreRequestId?: boolean;
			auxiliary?: (toolName: string, input: Record<string, unknown>) => { handled: true; value: unknown } | undefined;
		} = {},
	) {
		this.steps = steps;
	}

	private comparableInput(input: Record<string, unknown>): Record<string, unknown> {
		const copy = { ...input };
		if (this.options.ignoreRequestId) delete copy.requestId;
		if (copy.operation === "list" && copy.resource === "issue") delete copy.limit;
		return copy;
	}

	async execute(
		toolName: string,
		input: Record<string, unknown>,
		_signal: AbortSignal | undefined,
		_context: ForgeToolContext,
	): Promise<unknown> {
		const startedAt = Date.now();
		const step = this.steps[this.nextStep];
		const matches = step && step.toolName === toolName && stableJson(this.comparableInput(step.input)) === stableJson(this.comparableInput(input));
		if (!matches) {
			const auxiliary = this.options.auxiliary?.(toolName, input);
			if (auxiliary) {
				const finishedAt = Date.now();
				this.calls.push({ toolName, input, startedAt, finishedAt });
				return auxiliary.value;
			}
		}
		if (!step) {
			const reason = "no scripted response remains";
			this.unexpectedCalls.push({ toolName, input, reason });
			throw new Error(`Unexpected Forge service call ${toolName}: ${reason}`);
		}
		if (
			step.toolName !== toolName ||
			stableJson(this.comparableInput(step.input)) !== stableJson(this.comparableInput(input))
		) {
			const reason = `expected ${step.toolName} ${stableJson(step.input)}, got ${toolName} ${stableJson(input)}`;
			this.unexpectedCalls.push({ toolName, input, reason });
			throw new Error(
				`Unexpected Forge service call. ${reason}`,
			);
		}
		this.nextStep += 1;
		if (step.delayMs !== undefined) {
			await new Promise<void>((resolve, reject) => {
				const timer = setTimeout(resolve, step.delayMs);
				const onAbort = () => {
					clearTimeout(timer);
					reject(new DOMException("The operation was aborted", "AbortError"));
				};
				if (_signal?.aborted) return onAbort();
				_signal?.addEventListener("abort", onAbort, { once: true });
			});
		}
		const finishedAt = Date.now();
		this.calls.push({ toolName, input, startedAt, finishedAt });
		if (step.outcome.kind === "throw") {
			throw new ForgeError(step.outcome.code, step.outcome.message, step.outcome.details);
		}
		return step.outcome.value;
	}

	get consumedSteps(): number {
		return this.nextStep;
	}
}
