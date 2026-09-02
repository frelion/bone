import { randomUUID } from "node:crypto";
import {
	chmodSync,
	existsSync,
	mkdirSync,
	readFileSync,
	renameSync,
	statSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";

interface StoredCredential {
	type: "token";
	token: string;
}

type CredentialFile = Record<string, StoredCredential>;

export interface ResolvedForgeCredential {
	type: "token";
	/** Secret value. Never serialize or log this object. */
	token: string;
}

function resolvedToken(token: string): ResolvedForgeCredential {
	const credential = { type: "token" } as ResolvedForgeCredential;
	Object.defineProperty(credential, "token", { value: token, enumerable: false, writable: false });
	return Object.freeze(credential);
}

function parseFile(path: string): CredentialFile {
	if (!existsSync(path)) return {};
	if (process.platform !== "win32" && (statSync(path).mode & 0o077) !== 0) {
		throw new Error(`Refusing to read Forge credentials ${path}: permissions must be 0600`);
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(readFileSync(path, "utf8"));
	} catch (error) {
		throw new Error(`Failed to read Forge credentials ${path}: ${error instanceof Error ? error.message : error}`);
	}
	if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
		throw new Error(`Invalid Forge credentials ${path}: expected an object`);
	}
	const result: CredentialFile = {};
	for (const [key, value] of Object.entries(parsed)) {
		if (typeof value !== "object" || value === null || Array.isArray(value)) {
			throw new Error(`Invalid Forge credential ${key}`);
		}
		const entry = value as Record<string, unknown>;
		if (Object.keys(entry).some((field) => field !== "type" && field !== "token")) {
			throw new Error(`Invalid Forge credential ${key}: unknown field`);
		}
		if (entry.type !== "token" || typeof entry.token !== "string" || entry.token.length === 0) {
			throw new Error(`Invalid Forge credential ${key}`);
		}
		result[key] = { type: "token", token: entry.token };
	}
	return result;
}

export class ForgeCredentialStore {
	private readonly path: string;

	constructor(agentDir: string) {
		this.path = join(agentDir, "forge-auth.json");
	}

	resolve(key: string, env: NodeJS.ProcessEnv = process.env): ResolvedForgeCredential | undefined {
		const credential = parseFile(this.path)[key];
		if (!credential) return undefined;
		const match = /^\$([A-Z_][A-Z0-9_]*)$/.exec(credential.token);
		if (!match) return resolvedToken(credential.token);
		const token = env[match[1]];
		if (!token) throw new Error(`Forge credential ${key} references an unset environment variable`);
		return resolvedToken(token);
	}

	has(key: string): boolean {
		return parseFile(this.path)[key] !== undefined;
	}

	set(key: string, credential: StoredCredential): void {
		if (!key || !credential.token) throw new Error("Forge credential key and token are required");
		const data = parseFile(this.path);
		data[key] = credential;
		const directory = dirname(this.path);
		mkdirSync(directory, { recursive: true, mode: 0o700 });
		const temporary = `${this.path}.${randomUUID()}.tmp`;
		try {
			writeFileSync(temporary, `${JSON.stringify(data, null, 2)}\n`, { encoding: "utf8", mode: 0o600 });
			chmodSync(temporary, 0o600);
			renameSync(temporary, this.path);
			chmodSync(this.path, 0o600);
		} catch (error) {
			if (existsSync(temporary)) unlinkSync(temporary);
			throw new Error(`Failed to save Forge credentials: ${error instanceof Error ? error.message : error}`);
		}
	}

	remove(key: string): void {
		const data = parseFile(this.path);
		if (!(key in data)) return;
		delete data[key];
		const directory = dirname(this.path);
		mkdirSync(directory, { recursive: true, mode: 0o700 });
		const temporary = `${this.path}.${randomUUID()}.tmp`;
		try {
			writeFileSync(temporary, `${JSON.stringify(data, null, 2)}\n`, { encoding: "utf8", mode: 0o600 });
			chmodSync(temporary, 0o600);
			renameSync(temporary, this.path);
			chmodSync(this.path, 0o600);
		} catch (error) {
			if (existsSync(temporary)) unlinkSync(temporary);
			throw new Error(`Failed to remove Forge credential: ${error instanceof Error ? error.message : error}`);
		}
	}
}
