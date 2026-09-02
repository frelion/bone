import type { Api, Provider } from "@frelion/bone-ai";

/**
 * Non-sensitive provider metadata suitable for a selection UI or a draft
 * configuration. Credentials and request headers intentionally do not appear
 * here because either can contain secrets.
 */
export interface ProviderPreset {
	readonly id: string;
	readonly label: string;
	readonly baseUrl?: string;
	readonly api?: Api;
}

const COMMON_PROVIDER_IDS = [
	"openai",
	"anthropic",
	"deepseek",
	"openrouter",
	"kimi-coding",
	"google",
	"groq",
	"mistral",
	"minimax",
	"zai",
] as const;

const COMMON_PROVIDER_RANK = new Map<string, number>(COMMON_PROVIDER_IDS.map((id, index) => [id, index]));

const CUSTOM_OPENAI_COMPATIBLE_PRESET: ProviderPreset = {
	id: "custom",
	label: "Custom / OpenAI Compatible",
	api: "openai-completions",
};

function comparePresets(left: ProviderPreset, right: ProviderPreset): number {
	const leftRank = COMMON_PROVIDER_RANK.get(left.id);
	const rightRank = COMMON_PROVIDER_RANK.get(right.id);
	if (leftRank !== undefined || rightRank !== undefined) {
		if (leftRank === undefined) return 1;
		if (rightRank === undefined) return -1;
		if (leftRank !== rightRank) return leftRank - rightRank;
	}
	if (left.label !== right.label) return left.label < right.label ? -1 : 1;
	if (left.id !== right.id) return left.id < right.id ? -1 : 1;
	return 0;
}

/** Derive deterministic, secret-free provider choices from the active pi-ai providers. */
export function deriveProviderPresets(providers: readonly Provider[]): ProviderPreset[] {
	const presets: ProviderPreset[] = providers.map((provider) => ({
		id: provider.id,
		label: provider.name,
		baseUrl: provider.baseUrl,
		api: provider.getModels()[0]?.api,
	}));
	presets.push(CUSTOM_OPENAI_COMPATIBLE_PRESET);
	return presets.sort(comparePresets);
}
