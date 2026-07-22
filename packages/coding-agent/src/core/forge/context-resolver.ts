import { execFile } from "node:child_process";
import { promisify } from "node:util";
import type { ForgeRepositoryRef } from "./contracts.ts";
import { ForgeError } from "./errors.ts";

const execFileAsync = promisify(execFile);

function sanitizedRemoteUrl(remoteUrl: string): string {
	try {
		const url = new URL(remoteUrl);
		url.username = "";
		url.password = "";
		return url.toString();
	} catch {
		const scp = /^(?:[^@/]+@)?([^:/]+):(.+)$/.exec(remoteUrl);
		return scp ? `${scp[1]}:${scp[2]}` : "[invalid remote]";
	}
}

export function parseGitRemote(remoteUrl: string, remoteName = "origin", rootDir = process.cwd()): ForgeRepositoryRef {
	let host: string;
	let pathname: string;
	try {
		const url = new URL(remoteUrl);
		host = url.hostname;
		pathname = url.pathname;
	} catch {
		const scp = /^(?:[^@/]+@)?([^:/]+):(.+)$/.exec(remoteUrl);
		if (!scp) throw new ForgeError("validation_failed", "Unsupported Git remote URL");
		host = scp[1];
		pathname = scp[2];
	}
	const projectPath = pathname
		.replace(/^\/+/, "")
		.replace(/\.git\/?$/, "")
		.replace(/\/$/, "");
	if (!host || !projectPath || !projectPath.includes("/")) {
		throw new ForgeError("validation_failed", "Git remote does not identify a project");
	}
	return {
		provider: host.toLowerCase() === "github.com" ? "github" : "gitlab",
		host: host.toLowerCase(),
		projectPath,
		remoteName,
		remoteUrl: sanitizedRemoteUrl(remoteUrl),
		rootDir,
	};
}

export async function resolveForgeContext(cwd = process.cwd(), remoteName = "origin"): Promise<ForgeRepositoryRef> {
	try {
		const [{ stdout: rootOutput }, { stdout: remoteOutput }] = await Promise.all([
			execFileAsync("git", ["--no-optional-locks", "rev-parse", "--show-toplevel"], { cwd, encoding: "utf8" }),
			execFileAsync("git", ["--no-optional-locks", "remote", "get-url", remoteName], { cwd, encoding: "utf8" }),
		]);
		return parseGitRemote(remoteOutput.trim(), remoteName, rootOutput.trim());
	} catch (error) {
		if (error instanceof ForgeError) throw error;
		throw new ForgeError("not_found", `Unable to resolve Git remote '${remoteName}'`, {}, { cause: error });
	}
}
