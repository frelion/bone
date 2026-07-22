import { type Dispatcher, request } from "undici";
import { ForgeError, redactSecrets } from "./errors.ts";

export interface ForgeHttpClientOptions {
	baseUrl: string;
	provider: "gitlab" | "github";
	authHeaders: Readonly<Record<string, string>>;
	redactedSecrets: readonly string[];
	allowedHosts: readonly string[];
	dispatcher?: Dispatcher;
	allowInsecureHttp?: boolean;
	requestTimeoutMs?: number;
	maxResponseBytes?: number;
}

export interface ForgeHttpResponse<T> {
	status: number;
	headers: Record<string, string | string[] | undefined>;
	data: T;
}

const MAX_ERROR_BODY_LENGTH = 2_000;
const DEFAULT_MAX_RESPONSE_BYTES = 10 * 1024 * 1024;

async function readBoundedBody(
	body: AsyncIterable<Uint8Array>,
	contentLength: string | string[] | undefined,
	maximumBytes: number,
): Promise<string> {
	const declaredLength = typeof contentLength === "string" ? Number.parseInt(contentLength, 10) : Number.NaN;
	if (Number.isFinite(declaredLength) && declaredLength > maximumBytes) {
		throw new ForgeError("invalid_remote_response", `Forge API response exceeds ${maximumBytes} bytes`);
	}
	const chunks: Buffer[] = [];
	let total = 0;
	for await (const chunk of body) {
		total += chunk.byteLength;
		if (total > maximumBytes) {
			throw new ForgeError("invalid_remote_response", `Forge API response exceeds ${maximumBytes} bytes`);
		}
		chunks.push(Buffer.from(chunk));
	}
	return Buffer.concat(chunks, total).toString("utf8");
}

function normalizedHost(url: URL): string {
	return url.host.toLowerCase();
}

function statusCodeToError(status: number): ForgeError["code"] {
	if (status === 401) return "authentication_required";
	if (status === 403) return "permission_denied";
	if (status === 404) return "not_found";
	if (status === 409) return "conflict";
	if (status === 429) return "rate_limited";
	if (status === 400 || status === 422) return "validation_failed";
	return "remote_failure";
}

export class ForgeHttpClient {
	readonly baseUrl: URL;
	private readonly provider: "gitlab" | "github";
	private readonly authHeaders: Readonly<Record<string, string>>;
	private readonly redactedSecrets: readonly string[];
	private readonly dispatcher?: Dispatcher;
	private readonly requestTimeoutMs: number;
	private readonly allowedHosts: ReadonlySet<string>;
	private readonly maxResponseBytes: number;

	constructor(options: ForgeHttpClientOptions) {
		this.baseUrl = new URL(options.baseUrl);
		this.provider = options.provider;
		this.authHeaders = options.authHeaders;
		this.redactedSecrets = options.redactedSecrets;
		this.dispatcher = options.dispatcher;
		this.requestTimeoutMs = options.requestTimeoutMs ?? 30_000;
		this.allowedHosts = new Set(options.allowedHosts.map((host) => host.toLowerCase()));
		this.maxResponseBytes = options.maxResponseBytes ?? DEFAULT_MAX_RESPONSE_BYTES;
		if (!Number.isSafeInteger(this.maxResponseBytes) || this.maxResponseBytes <= 0) {
			throw new ForgeError("validation_failed", "Forge response size limit must be a positive integer");
		}

		if (this.baseUrl.protocol !== "https:" && !options.allowInsecureHttp) {
			throw new ForgeError("unsafe_remote", "Forge instances must use HTTPS");
		}
		if (!this.allowedHosts.has(normalizedHost(this.baseUrl))) {
			throw new ForgeError("unsafe_remote", `Forge host is not allowlisted: ${this.baseUrl.host}`);
		}
	}

	async request<T>(
		method: Dispatcher.HttpMethod,
		path: string,
		options: {
			query?: Record<string, string | number | boolean | undefined>;
			body?: unknown;
			signal?: AbortSignal;
		} = {},
	): Promise<ForgeHttpResponse<T>> {
		const requestUrl = new URL(path, "https://forge.invalid");
		const target = new URL(this.baseUrl);
		const basePath = target.pathname.replace(/\/$/, "");
		const requestPath =
			basePath.endsWith("/api/v4") && requestUrl.pathname.startsWith("/api/v4/")
				? requestUrl.pathname.slice("/api/v4".length)
				: requestUrl.pathname;
		target.pathname = `${basePath}${requestPath}`;
		target.search = requestUrl.search;
		if (!this.allowedHosts.has(normalizedHost(target)) || normalizedHost(target) !== normalizedHost(this.baseUrl)) {
			throw new ForgeError("unsafe_remote", `Cross-host Forge request rejected: ${target.host}`);
		}
		for (const [key, value] of Object.entries(options.query ?? {})) {
			if (value !== undefined) target.searchParams.set(key, String(value));
		}

		const headers: Record<string, string> = { accept: "application/json", ...this.authHeaders };
		let body: string | undefined;
		if (options.body !== undefined) {
			headers["content-type"] = "application/json";
			body = JSON.stringify(options.body);
		}

		try {
			const response = await request(target, {
				method,
				headers,
				body,
				dispatcher: this.dispatcher,
				headersTimeout: this.requestTimeoutMs,
				bodyTimeout: this.requestTimeoutMs,
				signal: options.signal,
			});

			if (response.statusCode >= 300 && response.statusCode < 400) {
				const location = response.headers.location;
				const destination = typeof location === "string" ? new URL(location, target) : undefined;
				await response.body.dump();
				throw new ForgeError(
					"unsafe_remote",
					destination && normalizedHost(destination) !== normalizedHost(target)
						? `Cross-host redirect rejected: ${destination.host}`
						: "Forge API redirects are not followed",
					{ status: response.statusCode, provider: this.provider, host: target.host },
				);
			}

			const raw = await readBoundedBody(response.body, response.headers["content-length"], this.maxResponseBytes);
			if (response.statusCode < 200 || response.statusCode >= 300) {
				const retryAfter = response.headers["retry-after"];
				throw new ForgeError(
					statusCodeToError(response.statusCode),
					redactSecrets(
						`Forge API returned ${response.statusCode}: ${raw.slice(0, MAX_ERROR_BODY_LENGTH)}`,
						this.redactedSecrets,
					),
					{
						status: response.statusCode,
						provider: this.provider,
						host: target.host,
						retryAfterSeconds:
							typeof retryAfter === "string" ? Number.parseInt(retryAfter, 10) || undefined : undefined,
					},
				);
			}

			let data: T;
			try {
				data = (raw.length === 0 ? undefined : JSON.parse(raw)) as T;
			} catch (error) {
				throw new ForgeError(
					"invalid_remote_response",
					"Forge API returned invalid JSON",
					{ status: response.statusCode },
					{ cause: error },
				);
			}
			return { status: response.statusCode, headers: response.headers, data };
		} catch (error) {
			if (error instanceof ForgeError) throw error;
			const message = error instanceof Error ? error.message : String(error);
			throw new ForgeError(
				"remote_failure",
				redactSecrets(`Forge request failed: ${message}`, this.redactedSecrets),
				{ provider: this.provider, host: target.host },
				{ cause: error },
			);
		}
	}
}
