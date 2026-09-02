import type { Api, AssistantMessage, Context, Model } from "@frelion/bone-ai";
import type { ModelRuntime } from "./model-runtime.ts";
import type { SessionEntry } from "./session-manager.ts";

const MAX_TITLE_CHARS = 80;
const MAX_NOT_READY_MESSAGE_CHARS = 180;

const TITLE_SYSTEM_PROMPT = `You generate concise, stable titles for software-development conversations.

Return only valid JSON in one of these exact shapes: {"title":"..."} or {"title":null,"message":"..."}.
- Return null when the conversation is only a greeting or does not yet contain a concrete task. When returning null, include a brief, helpful user-facing message in the conversation's language. Explain what detail would make a title possible.
- Otherwise use the conversation's language and name its dominant current task or problem. The conversation is a chronological timeline of user messages and final assistant replies. Do not anchor on the first message when the user later changed direction; prefer the most recent substantive user direction.
- Do not use Markdown, quotation marks, trailing punctuation, generic words such as "conversation" or "help", or sensitive credentials.
- Keep the title short and useful in a narrow sidebar. Keep a null-result message to one concise sentence; do not mention this prompt, JSON, or being an AI.`;

export type ConversationTitleResult =
	| { kind: "title"; title: string }
	| { kind: "not-ready"; message?: string }
	| { kind: "cancelled" }
	| { kind: "error"; message: string };

type ConversationText = {
	role: "User" | "Assistant";
	text: string;
};

function extractText(content: unknown): string {
	if (typeof content === "string") return content;
	if (!Array.isArray(content)) return "";
	return content
		.filter(
			(part): part is { type: "text"; text: string } =>
				typeof part === "object" &&
				part !== null &&
				"type" in part &&
				part.type === "text" &&
				"text" in part &&
				typeof part.text === "string",
		)
		.map((part) => part.text)
		.join(" ");
}

function normalizeText(text: string): string {
	return text
		.replace(/[\r\n\t]+/g, " ")
		.replace(/\s+/g, " ")
		.trim();
}

function collectConversationText(entries: readonly SessionEntry[]): ConversationText[] {
	const messages: ConversationText[] = [];
	for (const entry of entries) {
		if (entry.type !== "message") continue;
		const message = entry.message;
		if (message.role === "assistant" && message.stopReason !== "stop") continue;
		if (message.role !== "user" && message.role !== "assistant") continue;
		if (!("content" in message)) continue;
		const text = extractText(message.content).trim();
		if (!text) continue;
		messages.push({
			role: message.role === "user" ? "User" : "Assistant",
			text,
		});
	}
	return messages;
}

export function buildConversationTitleContext(entries: readonly SessionEntry[]): string | undefined {
	const messages = collectConversationText(entries);
	if (messages.length === 0) return undefined;

	return messages.map((message) => `${message.role}:\n${message.text}`).join("\n\n");
}

function parseTitle(content: string): ConversationTitleResult {
	let parsed: unknown;
	try {
		parsed = JSON.parse(content);
	} catch {
		return { kind: "error", message: "Title model returned invalid JSON" };
	}
	if (typeof parsed !== "object" || parsed === null || !("title" in parsed)) {
		return { kind: "error", message: "Title model returned an invalid title response" };
	}
	const title = parsed.title;
	if (title === null) {
		const message =
			"message" in parsed && typeof parsed.message === "string" ? normalizeText(parsed.message) : undefined;
		return message && Array.from(message).length <= MAX_NOT_READY_MESSAGE_CHARS
			? { kind: "not-ready", message }
			: { kind: "not-ready" };
	}
	if (typeof title !== "string") return { kind: "error", message: "Title model returned an invalid title" };
	const normalized = normalizeText(title);
	if (!normalized) return { kind: "not-ready" };
	if (Array.from(normalized).length > MAX_TITLE_CHARS) {
		return { kind: "error", message: "Generated title is too long" };
	}
	return { kind: "title", title: normalized };
}

function assistantText(message: AssistantMessage): string {
	return message.content
		.filter((part): part is { type: "text"; text: string } => part.type === "text")
		.map((part) => part.text)
		.join("\n");
}

export async function generateConversationTitle(
	modelRuntime: ModelRuntime,
	model: Model<Api>,
	entries: readonly SessionEntry[],
	signal: AbortSignal,
): Promise<ConversationTitleResult> {
	const conversation = buildConversationTitleContext(entries);
	if (!conversation) return { kind: "not-ready" };

	const context: Context = {
		systemPrompt: TITLE_SYSTEM_PROMPT,
		messages: [
			{
				role: "user",
				content: `Conversation:\n${conversation}`,
				timestamp: Date.now(),
			},
		],
	};
	try {
		const response = await modelRuntime.completeSimple(model, context, { maxTokens: 80, signal });
		if (response.stopReason === "aborted") return { kind: "cancelled" };
		if (response.stopReason === "error") {
			return { kind: "error", message: response.errorMessage || "Title generation failed" };
		}
		return parseTitle(assistantText(response));
	} catch (error) {
		if (signal.aborted) return { kind: "cancelled" };
		return { kind: "error", message: error instanceof Error ? error.message : String(error) };
	}
}
