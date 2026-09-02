import type { Api, Model } from "@frelion/bone-ai";
import { resolveCloudflareModel } from "@frelion/bone-ai/providers/cloudflare-stream";
import { describe, expect, it } from "vitest";
import { AuthStorage } from "../src/core/auth-storage.ts";
import { ModelRegistry } from "../src/core/model-registry.ts";
import { ModelRuntime } from "../src/core/model-runtime.ts";

type PreparedRequest = {
	model: Model<Api>;
	options: { apiKey?: string; headers?: Record<string, string>; env?: Record<string, string> };
};

async function prepare(modelRuntime: ModelRuntime, model: Model<Api>): Promise<PreparedRequest> {
	const runtime = modelRuntime as unknown as {
		prepareRequest(model: Model<Api>, options?: unknown): Promise<PreparedRequest>;
	};
	return runtime.prepareRequest(model);
}

async function createCloudflareRuntime(): Promise<{ modelRuntime: ModelRuntime; modelRegistry: ModelRegistry }> {
	const authStorage = AuthStorage.inMemory();
	await authStorage.modify("cloudflare-ai-gateway", async () => ({
		type: "api_key",
		key: "test-token",
		env: {
			CLOUDFLARE_ACCOUNT_ID: "test-account",
			CLOUDFLARE_GATEWAY_ID: "test-gateway",
		},
	}));
	const modelRuntime = await ModelRuntime.create({ credentials: authStorage, modelsPath: null });
	return { modelRuntime, modelRegistry: new ModelRegistry(modelRuntime) };
}

describe("ModelRegistry Cloudflare compat streaming", () => {
	it("materializes the Cloudflare endpoint through ModelRuntime streaming", async () => {
		const { modelRuntime } = await createCloudflareRuntime();
		const model = modelRuntime.getModel("cloudflare-ai-gateway", "workers-ai/@cf/moonshotai/kimi-k2.5");
		expect(model).toBeDefined();

		const prepared = await prepare(modelRuntime, model!);
		expect(resolveCloudflareModel(prepared.model, prepared.options.env).baseUrl).toBe(
			"https://gateway.ai.cloudflare.com/v1/test-account/test-gateway/compat",
		);
		expect(prepared.options.headers?.["cf-aig-authorization"]).toBe("Bearer test-token");
	});

	it("materializes the Cloudflare endpoint after extension-style auth resolution", async () => {
		const { modelRuntime, modelRegistry } = await createCloudflareRuntime();
		const model = modelRegistry.find("cloudflare-ai-gateway", "workers-ai/@cf/moonshotai/kimi-k2.5");
		expect(model).toBeDefined();

		const auth = await modelRegistry.getApiKeyAndHeaders(model!);
		expect(auth.ok).toBe(true);
		if (!auth.ok) throw new Error(auth.error);

		expect(auth.headers?.["cf-aig-authorization"]).toBe("Bearer test-token");
		const prepared = await prepare(modelRuntime, model!);
		expect(resolveCloudflareModel(prepared.model, prepared.options.env).baseUrl).toBe(
			"https://gateway.ai.cloudflare.com/v1/test-account/test-gateway/compat",
		);
	});
});
