import { anthropicOAuth } from "./auth/oauth/anthropic.ts";
import { registerBundledOAuthModule } from "./auth/oauth/load.ts";
import { openaiCodexOAuth } from "./auth/oauth/openai-codex.ts";
import { xaiOAuth } from "./auth/oauth/xai.ts";

export function registerBunOAuthModules(): void {
	registerBundledOAuthModule("./anthropic.ts", { anthropicOAuth });
	registerBundledOAuthModule("./openai-codex.ts", { openaiCodexOAuth });
	registerBundledOAuthModule("./xai.ts", { xaiOAuth });
}
