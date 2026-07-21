import { type Api, createProvider, type Model, type Provider } from "@frelion/bone-ai";
import { describe, expect, it } from "vitest";
import { deriveProviderPresets } from "../src/core/provider-presets.ts";

function model(provider: string, api: Api): Model<Api> {
	return {
		id: "test-model",
		name: "Test model",
		api,
		provider,
		baseUrl: "https://example.test/v1",
		reasoning: false,
		input: ["text"],
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
		contextWindow: 1,
		maxTokens: 1,
	};
}

function provider(id: string, name: string, api?: Api): Provider {
	return createProvider({
		id,
		name,
		baseUrl: `https://${id}.example.test/v1`,
		headers: { "x-provider-secret": "must-not-appear" },
		auth: { apiKey: { name: "Test key", resolve: async () => ({ auth: { apiKey: "must-not-appear" } }) } },
		models: api ? [model(id, api)] : [],
		api: {
			stream: () => {
				throw new Error("not used");
			},
			streamSimple: () => {
				throw new Error("not used");
			},
		},
	});
}

describe("provider presets", () => {
	it("derives secret-free metadata, infers the API, and orders common providers first", () => {
		const presets = deriveProviderPresets([
			provider("zebra", "Zebra", "openai-completions"),
			provider("mistral", "Mistral", "mistral-conversations"),
			provider("anthropic", "Anthropic", "anthropic-messages"),
			provider("openai", "OpenAI", "openai-responses"),
			provider("alpha", "Alpha"),
		]);

		expect(presets.map((preset) => preset.id)).toEqual([
			"openai",
			"anthropic",
			"mistral",
			"alpha",
			"custom",
			"zebra",
		]);
		expect(presets[0]).toEqual({
			id: "openai",
			label: "OpenAI",
			baseUrl: "https://openai.example.test/v1",
			api: "openai-responses",
		});
		expect(presets.find((preset) => preset.id === "alpha")?.api).toBeUndefined();
		expect(presets.find((preset) => preset.id === "custom")).toEqual({
			id: "custom",
			label: "Custom / OpenAI Compatible",
			api: "openai-completions",
		});
		expect(JSON.stringify(presets)).not.toContain("must-not-appear");
	});
});
