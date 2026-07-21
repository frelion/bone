import type { ApiKeyCredential, ProviderHeaders } from "@frelion/bone-ai";

export type OpenAICompatibleApi = "openai-completions" | "openai-responses";

/** In-memory inputs for a one-off OpenAI-compatible GET /models request. */
export interface OpenAICompatibleModelDiscoveryInput {
	baseUrl: string;
	api: OpenAICompatibleApi;
	credential?: ApiKeyCredential;
	headers?: ProviderHeaders;
	authHeader?: boolean;
	timeoutMs?: number;
	signal?: AbortSignal;
}

function modelsUrl(baseUrl: string): URL {
	if (!baseUrl.trim()) {
		throw new Error("Enter a base URL before discovering models.");
	}

	let url: URL;
	try {
		url = new URL(baseUrl);
	} catch {
		throw new Error("Enter a valid base URL before discovering models.");
	}
	url.pathname = `${url.pathname.replace(/\/+$/, "")}/models`;
	url.search = "";
	url.hash = "";
	return url;
}

function requestHeaders(
	headers: ProviderHeaders | undefined,
	credential: ApiKeyCredential | undefined,
	authHeader: boolean,
): Headers {
	const result = new Headers({ accept: "application/json" });
	for (const [name, value] of Object.entries(headers ?? {})) {
		if (value === null) result.delete(name);
		else result.set(name, value);
	}
	if (authHeader) {
		const key = credential?.key?.trim();
		if (!key) {
			throw new Error("Enter an API key before discovering models.");
		}
		result.set("authorization", `Bearer ${key}`);
	}
	return result;
}

function discoverySignal(timeoutMs: number | undefined, signal: AbortSignal | undefined): AbortSignal | undefined {
	if (timeoutMs === undefined) return signal;
	if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
		throw new Error("Use a positive timeout before discovering models.");
	}
	const timeoutSignal = AbortSignal.timeout(timeoutMs);
	return signal ? AbortSignal.any([signal, timeoutSignal]) : timeoutSignal;
}

function parseModelIds(value: unknown): string[] {
	if (typeof value !== "object" || value === null || !("data" in value) || !Array.isArray(value.data)) {
		throw new Error("The model endpoint returned invalid JSON. Verify that it implements GET /models.");
	}

	const ids = value.data.flatMap((entry) =>
		typeof entry === "object" && entry !== null && "id" in entry && typeof entry.id === "string" && entry.id.trim()
			? [entry.id]
			: [],
	);
	return Array.from(new Set(ids)).sort((left, right) => (left === right ? 0 : left < right ? -1 : 1));
}

/**
 * Discover models from an OpenAI-compatible endpoint without persisting the
 * URL, credentials, headers, or returned model IDs.
 */
export async function discoverOpenAICompatibleModelIds(input: OpenAICompatibleModelDiscoveryInput): Promise<string[]> {
	if (input.api !== "openai-completions" && input.api !== "openai-responses") {
		throw new Error(
			"Model discovery supports only OpenAI Completions or Responses. Select an OpenAI-compatible API.",
		);
	}

	const url = modelsUrl(input.baseUrl);
	const headers = requestHeaders(input.headers, input.credential, input.authHeader ?? false);
	const signal = discoverySignal(input.timeoutMs, input.signal);

	let response: Response;
	try {
		response = await fetch(url, { method: "GET", headers, signal });
	} catch (error) {
		if (signal?.aborted) {
			throw new Error("Model discovery was cancelled or timed out. Check the endpoint or try again.", {
				cause: error,
			});
		}
		throw new Error("Could not reach the model endpoint. Check the base URL and network connection.", {
			cause: error,
		});
	}

	if (!response.ok) {
		throw new Error(
			`Model discovery failed with HTTP ${response.status}. Check the base URL, API key, and access permissions.`,
		);
	}

	let payload: unknown;
	try {
		payload = await response.json();
	} catch (error) {
		throw new Error("The model endpoint returned invalid JSON. Verify that it implements GET /models.", {
			cause: error,
		});
	}
	return parseModelIds(payload);
}
