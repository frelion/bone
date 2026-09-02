import { EventStream, type AssistantMessage, type AssistantMessageEvent, type Message, type Model } from "@frelion/bone-ai";
import type { AgentMessage, StreamFn } from "../../../agent/src/types.ts";
import type { EvalAssistantStep } from "./types.ts";

const model: Model<"openai-responses"> = {
	id: "forge-eval-scripted",
	name: "Forge eval scripted model",
	api: "openai-responses",
	provider: "forge-eval",
	baseUrl: "http://forge-eval.invalid",
	reasoning: false,
	input: ["text"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	contextWindow: 128_000,
	maxTokens: 2_048,
};

function messageForStep(step: EvalAssistantStep, index: number): AssistantMessage {
	const content: AssistantMessage["content"] = [];
	if (step.text !== undefined) content.push({ type: "text", text: step.text });
	for (const [callIndex, call] of (step.toolCalls ?? []).entries()) {
		content.push({
			type: "toolCall",
			id: call.id ?? `forge_eval_call_${index}_${callIndex}`,
			name: call.name,
			arguments: call.args,
		});
	}
	const stopReason = step.toolCalls && step.toolCalls.length > 0 ? "toolUse" : "stop";
	return {
		role: "assistant",
		content: content.length > 0 ? content : [{ type: "text", text: "" }],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason,
		timestamp: Date.now(),
	};
}

function streamMessage(message: AssistantMessage): EventStream<AssistantMessageEvent, AssistantMessage> {
	const stream = new EventStream<AssistantMessageEvent, AssistantMessage>(
		(event) => event.type === "done" || event.type === "error",
		(event) => (event.type === "done" ? event.message : event.error),
	);
	queueMicrotask(() => stream.push({ type: "done", reason: message.stopReason, message }));
	return stream;
}

export interface ScriptedStreamResult {
	streamFn: StreamFn;
	get modelStepCount(): number;
	get scriptExhausted(): boolean;
	readonly contexts: Array<{ step: number; messageCount: number; bytes: number; hasToolResult: boolean }>;
}

export function createScriptedStream(steps: readonly EvalAssistantStep[]): ScriptedStreamResult {
	let stepIndex = 0;
	let scriptExhausted = false;
	const contexts: ScriptedStreamResult["contexts"] = [];
	const streamFn: StreamFn = async (_requestedModel, context) => {
		const bytes = Buffer.byteLength(JSON.stringify(context.messages), "utf8");
		contexts.push({
			step: stepIndex,
			messageCount: context.messages.length,
			bytes,
			hasToolResult: context.messages.some((message) => message.role === "toolResult"),
		});
		const step = steps[stepIndex];
		stepIndex++;
		if (!step) {
			scriptExhausted = true;
			return streamMessage(messageForStep({ text: "__FORGE_EVAL_SCRIPT_EXHAUSTED__" }, stepIndex));
		}
		return streamMessage(messageForStep(step, stepIndex));
	};
	return {
		streamFn,
		get modelStepCount() {
			return stepIndex;
		},
		get scriptExhausted() {
			return scriptExhausted;
		},
		contexts,
	};
}

export function identityEvalConverter(messages: AgentMessage[]): Message[] {
	return messages.filter(
		(message): message is Extract<AgentMessage, { role: "user" | "assistant" | "toolResult" }> =>
			message.role === "user" || message.role === "assistant" || message.role === "toolResult",
	) as Message[];
}

export { model as forgeEvalModel };
