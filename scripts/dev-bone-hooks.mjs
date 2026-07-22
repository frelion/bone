import { existsSync } from "node:fs";
import { isAbsolute, join, relative, resolve } from "node:path";

export function isBoneManagedHooksPath(configuredHooksPath, { repoRoot, devRoot }) {
	if (!configuredHooksPath) return false;
	const resolvedHooksPath = isAbsolute(configuredHooksPath)
		? configuredHooksPath
		: resolve(repoRoot, configuredHooksPath);
	const relativeManagedPath = relative(join(devRoot, "hooks"), resolvedHooksPath);
	return (
		relativeManagedPath.length > 0 &&
		!relativeManagedPath.startsWith("..") &&
		!isAbsolute(relativeManagedPath) &&
		existsSync(join(resolvedHooksPath, "config.json"))
	);
}
