import type { Api, ProviderStreams } from "../types.ts";
import { anthropicMessagesApi } from "./anthropic-messages.lazy.ts";
import { azureOpenAIResponsesApi } from "./azure-openai-responses.lazy.ts";
import { bedrockConverseStreamApi } from "./bedrock-converse-stream.lazy.ts";
import { googleGenerativeAIApi } from "./google-generative-ai.lazy.ts";
import { googleVertexApi } from "./google-vertex.lazy.ts";
import { mistralConversationsApi } from "./mistral-conversations.lazy.ts";
import { openAICodexResponsesApi } from "./openai-codex-responses.lazy.ts";
import { openAICompletionsApi } from "./openai-completions.lazy.ts";
import { openAIResponsesApi } from "./openai-responses.lazy.ts";
import { piMessagesApi } from "./pi-messages.lazy.ts";

const BUILTIN_APIS: ReadonlyMap<Api, ProviderStreams> = new Map([
	["anthropic-messages", anthropicMessagesApi()],
	["openai-completions", openAICompletionsApi()],
	["openai-responses", openAIResponsesApi()],
	["openai-codex-responses", openAICodexResponsesApi()],
	["azure-openai-responses", azureOpenAIResponsesApi()],
	["google-generative-ai", googleGenerativeAIApi()],
	["google-vertex", googleVertexApi()],
	["mistral-conversations", mistralConversationsApi()],
	["bedrock-converse-stream", bedrockConverseStreamApi()],
	["pi-messages", piMessagesApi()],
]);

/** Return the built-in stream implementation for an API identifier. */
export function getBuiltinApi(api: Api): ProviderStreams | undefined {
	return BUILTIN_APIS.get(api);
}
