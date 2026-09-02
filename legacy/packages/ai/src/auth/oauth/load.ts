import type { OAuthAuth } from "../types.ts";

type BundledOAuthModule = Record<string, unknown>;
type OAuthModuleRegistryGlobal = typeof globalThis & {
	__boneBundledOAuthModules?: Map<string, BundledOAuthModule>;
};

const oauthModuleRegistry = (() => {
	const global = globalThis as OAuthModuleRegistryGlobal;
	if (!global.__boneBundledOAuthModules) global.__boneBundledOAuthModules = new Map();
	return global.__boneBundledOAuthModules;
})();

export function registerBundledOAuthModule(specifier: string, module: BundledOAuthModule): void {
	oauthModuleRegistry.set(specifier, module);
}

/**
 * Loads an OAuth flow module through a variable specifier so bundlers cannot
 * follow the import into Node-only flow code (`node:http` callback servers,
 * `node:crypto` PKCE). The `.ts`/`.js` rewrite keeps the trick working from
 * both source and built output.
 */
const importOAuthModule = (specifier: string): Promise<unknown> => {
	const bundledModule = oauthModuleRegistry.get(specifier);
	if (bundledModule) return Promise.resolve(bundledModule);
	const runtimeSpecifier = import.meta.url.endsWith(".ts") ? specifier : specifier.replace(/\.ts$/, ".js");
	return import(runtimeSpecifier);
};

export const loadAnthropicOAuth = async (): Promise<OAuthAuth> =>
	((await importOAuthModule("./anthropic.ts")) as { anthropicOAuth: OAuthAuth }).anthropicOAuth;

export const loadOpenAICodexOAuth = async (): Promise<OAuthAuth> =>
	((await importOAuthModule("./openai-codex.ts")) as { openaiCodexOAuth: OAuthAuth }).openaiCodexOAuth;

export const loadGitHubCopilotOAuth = async (): Promise<OAuthAuth> =>
	((await importOAuthModule("./github-copilot.ts")) as { githubCopilotOAuth: OAuthAuth }).githubCopilotOAuth;

export const loadXaiOAuth = async (): Promise<OAuthAuth> =>
	((await importOAuthModule("./xai.ts")) as { xaiOAuth: OAuthAuth }).xaiOAuth;

export const loadRadiusOAuth = async (options: { name: string; gateway: string }): Promise<OAuthAuth> =>
	(
		(await importOAuthModule("./radius.ts")) as {
			createRadiusOAuth: (input: { name: string; gateway: string }) => OAuthAuth;
		}
	).createRadiusOAuth(options);
