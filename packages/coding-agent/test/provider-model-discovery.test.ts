import { afterEach, describe, expect, it, vi } from "vitest";
import { discoverOpenAICompatibleModelIds } from "../src/core/provider-model-discovery.ts";

afterEach(() => vi.restoreAllMocks());

describe("OpenAI-compatible model discovery", () => {
	it("normalizes GET /models, applies request-only auth, and returns sorted unique IDs", async () => {
		const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValue(
			new Response(JSON.stringify({ data: [{ id: "zeta" }, { id: "alpha" }, { id: "zeta" }, { id: 1 }] }), {
				status: 200,
			}),
		);

		await expect(
			discoverOpenAICompatibleModelIds({
				baseUrl: "https://example.test/v1/",
				api: "openai-completions",
				credential: { type: "api_key", key: "test-key" },
				headers: { "x-client": "test", authorization: "replaced" },
				authHeader: true,
				timeoutMs: 1000,
			}),
		).resolves.toEqual(["alpha", "zeta"]);

		const [url, options] = fetchSpy.mock.calls[0] ?? [];
		expect(url).toEqual(new URL("https://example.test/v1/models"));
		expect(options?.method).toBe("GET");
		expect(options?.signal).toBeInstanceOf(AbortSignal);
		const headers = new Headers(options?.headers);
		expect(headers.get("accept")).toBe("application/json");
		expect(headers.get("x-client")).toBe("test");
		expect(headers.get("authorization")).toBe("Bearer test-key");
	});

	it("supports keyless endpoints when bearer authentication is disabled", async () => {
		const fetchSpy = vi
			.spyOn(globalThis, "fetch")
			.mockResolvedValue(new Response(JSON.stringify({ data: [] }), { status: 200 }));

		await expect(
			discoverOpenAICompatibleModelIds({
				baseUrl: "https://example.test",
				api: "openai-responses",
				headers: { authorization: "Custom token" },
				authHeader: false,
			}),
		).resolves.toEqual([]);

		expect(new Headers(fetchSpy.mock.calls[0]?.[1]?.headers).get("authorization")).toBe("Custom token");
	});

	it("rejects incompatible protocols and incomplete connection details before making a request", async () => {
		const fetchSpy = vi.spyOn(globalThis, "fetch");

		await expect(
			discoverOpenAICompatibleModelIds({
				baseUrl: "https://example.test",
				api: "anthropic-messages" as "openai-completions",
			}),
		).rejects.toThrow("supports only OpenAI Completions or Responses");
		await expect(discoverOpenAICompatibleModelIds({ baseUrl: "", api: "openai-completions" })).rejects.toThrow(
			"Enter a base URL",
		);
		await expect(
			discoverOpenAICompatibleModelIds({
				baseUrl: "https://example.test",
				api: "openai-completions",
				authHeader: true,
			}),
		).rejects.toThrow("Enter an API key");
		expect(fetchSpy).not.toHaveBeenCalled();
	});

	it("reports non-OK responses and malformed model payloads with corrective errors", async () => {
		vi.spyOn(globalThis, "fetch")
			.mockResolvedValueOnce(new Response("unauthorized", { status: 401 }))
			.mockResolvedValueOnce(new Response("not json", { status: 200 }))
			.mockResolvedValueOnce(new Response(JSON.stringify({ models: [] }), { status: 200 }));

		const input = { baseUrl: "https://example.test/v1", api: "openai-completions" as const };
		await expect(discoverOpenAICompatibleModelIds(input)).rejects.toThrow("HTTP 401");
		await expect(discoverOpenAICompatibleModelIds(input)).rejects.toThrow("invalid JSON");
		await expect(discoverOpenAICompatibleModelIds(input)).rejects.toThrow("implements GET /models");
	});
});
