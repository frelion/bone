import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { applyHttpProxySettings } from "../src/core/http-dispatcher.ts";

const PROXY_ENV_KEYS = [
	"HTTP_PROXY",
	"HTTPS_PROXY",
	"NO_PROXY",
	"ALL_PROXY",
	"http_proxy",
	"https_proxy",
	"no_proxy",
	"all_proxy",
] as const;
const fixturePath = fileURLToPath(new URL("fixtures/http-dispatcher-bun.ts", import.meta.url));

function runBunFixture(mode: "proxy" | "sse"): Record<string, unknown> {
	const env = { ...process.env };
	for (const key of PROXY_ENV_KEYS) {
		delete env[key];
	}
	const result = spawnSync(process.execPath, [fixturePath, mode], {
		encoding: "utf8",
		env,
		timeout: 3_000,
	});
	expect(result.error).toBeUndefined();
	expect(result.status, result.stderr).toBe(0);
	return JSON.parse(result.stdout.trim()) as Record<string, unknown>;
}

describe("http proxy settings", () => {
	let savedEnv: Record<(typeof PROXY_ENV_KEYS)[number], string | undefined>;

	beforeEach(() => {
		savedEnv = Object.fromEntries(PROXY_ENV_KEYS.map((key) => [key, process.env[key]])) as Record<
			(typeof PROXY_ENV_KEYS)[number],
			string | undefined
		>;
		for (const key of PROXY_ENV_KEYS) {
			delete process.env[key];
		}
	});

	afterEach(() => {
		for (const key of PROXY_ENV_KEYS) {
			const value = savedEnv[key];
			if (value === undefined) {
				delete process.env[key];
			} else {
				process.env[key] = value;
			}
		}
	});

	it("applies httpProxy to HTTP_PROXY and HTTPS_PROXY", () => {
		applyHttpProxySettings("http://127.0.0.1:7890");

		expect(process.env.HTTP_PROXY).toBe("http://127.0.0.1:7890");
		expect(process.env.HTTPS_PROXY).toBe("http://127.0.0.1:7890");
	});

	it("does not override existing proxy env vars", () => {
		process.env.HTTP_PROXY = "http://env-http:8080";
		process.env.HTTPS_PROXY = "http://env-https:8080";

		applyHttpProxySettings("http://settings:7890");

		expect(process.env.HTTP_PROXY).toBe("http://env-http:8080");
		expect(process.env.HTTPS_PROXY).toBe("http://env-https:8080");
	});

	it("ignores empty values", () => {
		applyHttpProxySettings("   ");

		expect(process.env.HTTP_PROXY).toBeUndefined();
		expect(process.env.HTTPS_PROXY).toBeUndefined();
	});

	it("keeps Bun's native fetch for streaming responses", () => {
		const result = runBunFixture("sse");

		expect(result.fetchPreserved).toBe(true);
		expect(result.firstChunk).toContain("response.created");
		expect(result.firstChunk).not.toContain("response.completed");
		expect(result.body).toContain("response.completed");
	});

	it("keeps proxy support through Bun's native fetch", () => {
		const result = runBunFixture("proxy");

		expect(result.fetchPreserved).toBe(true);
		expect(result.body).toBe("proxied");
		expect(result.forgeBody).toBe("proxied");
		expect(result.proxiedUrls).toEqual(["http://bone-proxy-check.invalid/path?q=1"]);
		expect(result.tunneledHosts).toEqual(["bone-forge-proxy-check.invalid:80"]);
	});
});
