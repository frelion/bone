import { randomUUID } from "node:crypto";
import { chmodSync, existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import type { ForgeProvider } from "./contracts.ts";

export interface ForgeInstanceConfig {
	provider: ForgeProvider;
	host: string;
	apiBaseUrl: string;
	credential?: string;
	allowPrivateNetwork: boolean;
}

export interface ForgeConfig {
	instances: readonly ForgeInstanceConfig[];
}

function object(value: unknown, location: string): Record<string, unknown> {
	if (typeof value !== "object" || value === null || Array.isArray(value)) {
		throw new Error(`Invalid Forge configuration at ${location}: expected an object`);
	}
	return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, allowed: readonly string[], location: string): void {
	for (const key of Object.keys(value)) {
		if (!allowed.includes(key)) throw new Error(`Invalid Forge configuration at ${location}: unknown key ${key}`);
	}
}

function parseInstance(value: unknown, index: number): ForgeInstanceConfig {
	const location = `instances[${index}]`;
	const entry = object(value, location);
	exactKeys(entry, ["provider", "host", "apiBaseUrl", "credential", "allowPrivateNetwork"], location);
	if (entry.provider !== "gitlab" && entry.provider !== "github") {
		throw new Error(`Invalid Forge configuration at ${location}.provider`);
	}
	if (typeof entry.host !== "string" || entry.host.length === 0) {
		throw new Error(`Invalid Forge configuration at ${location}.host`);
	}
	if (typeof entry.apiBaseUrl !== "string") {
		throw new Error(`Invalid Forge configuration at ${location}.apiBaseUrl`);
	}
	let apiUrl: URL;
	try {
		apiUrl = new URL(entry.apiBaseUrl);
	} catch {
		throw new Error(`Invalid Forge configuration at ${location}.apiBaseUrl`);
	}
	if (apiUrl.protocol !== "https:" || apiUrl.username || apiUrl.password || apiUrl.search || apiUrl.hash) {
		throw new Error(`Invalid Forge configuration at ${location}.apiBaseUrl: HTTPS URL without credentials required`);
	}
	const host = entry.host.toLowerCase();
	const apiHost = apiUrl.hostname.toLowerCase();
	const isGitHubPublicApi = entry.provider === "github" && host === "github.com" && apiHost === "api.github.com";
	if (apiHost !== host && !isGitHubPublicApi) {
		throw new Error(`Invalid Forge configuration at ${location}: host and apiBaseUrl hostname must match`);
	}
	if (entry.credential !== undefined && (typeof entry.credential !== "string" || entry.credential.length === 0)) {
		throw new Error(`Invalid Forge configuration at ${location}.credential`);
	}
	if (entry.allowPrivateNetwork !== undefined && typeof entry.allowPrivateNetwork !== "boolean") {
		throw new Error(`Invalid Forge configuration at ${location}.allowPrivateNetwork`);
	}
	return {
		provider: entry.provider,
		host,
		apiBaseUrl: apiUrl.toString().replace(/\/$/, ""),
		credential: entry.credential,
		allowPrivateNetwork: entry.allowPrivateNetwork === true,
	};
}

export function parseForgeConfig(content: string, source = "Forge configuration"): ForgeConfig {
	let parsed: unknown;
	try {
		parsed = JSON.parse(content);
	} catch (error) {
		throw new Error(`Failed to parse ${source}: ${error instanceof Error ? error.message : error}`);
	}
	const root = object(parsed, "root");
	exactKeys(root, ["version", "instances"], "root");
	if (root.version !== 1) throw new Error("Invalid Forge configuration at version: expected 1");
	if (!Array.isArray(root.instances)) throw new Error("Invalid Forge configuration at instances: expected an array");
	const instances = root.instances.map(parseInstance);
	const identities = new Set<string>();
	for (const instance of instances) {
		const identity = `${instance.provider}:${instance.host}`;
		if (identities.has(identity)) throw new Error(`Duplicate Forge instance ${identity}`);
		identities.add(identity);
	}
	return { instances };
}

export function loadForgeConfig(agentDir: string): ForgeConfig {
	const path = join(agentDir, "forge.json");
	try {
		return parseForgeConfig(readFileSync(path, "utf8"), `Forge configuration ${path}`);
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") return { instances: [] };
		throw error;
	}
}

export function saveForgeConfig(agentDir: string, config: ForgeConfig): void {
	const validated = parseForgeConfig(JSON.stringify({ version: 1, instances: config.instances }));
	const path = join(agentDir, "forge.json");
	mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
	const temporary = `${path}.${randomUUID()}.tmp`;
	try {
		writeFileSync(temporary, `${JSON.stringify({ version: 1, instances: validated.instances }, null, 2)}\n`, {
			encoding: "utf8",
			mode: 0o600,
		});
		chmodSync(temporary, 0o600);
		renameSync(temporary, path);
		chmodSync(path, 0o600);
	} catch (error) {
		if (existsSync(temporary)) unlinkSync(temporary);
		throw new Error(`Failed to save Forge configuration: ${error instanceof Error ? error.message : error}`);
	}
}

export function findForgeInstance(
	config: ForgeConfig,
	provider: ForgeProvider,
	host: string,
): ForgeInstanceConfig | undefined {
	return config.instances.find((instance) => instance.provider === provider && instance.host === host.toLowerCase());
}

export function resolveForgeProvider(config: ForgeConfig, host: string, fallback: ForgeProvider): ForgeProvider {
	const configuredForHost = config.instances.filter((instance) => instance.host === host.toLowerCase());
	return configuredForHost.length === 1 ? configuredForHost[0].provider : fallback;
}
