import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DefaultResourceLoader } from "../src/core/resource-loader.ts";
import { handlePackageCommand } from "../src/package-manager-cli.ts";

describe("Bone package command boundary", () => {
	afterEach(() => {
		process.exitCode = undefined;
		vi.restoreAllMocks();
	});

	it.each(["install", "remove", "uninstall", "list", "config"])("rejects the removed %s command", async (command) => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		await expect(handlePackageCommand([command])).resolves.toBe(true);
		expect(process.exitCode).toBe(1);
	});

	it("rejects extension and package update targets", async () => {
		vi.spyOn(console, "error").mockImplementation(() => {});
		await expect(handlePackageCommand(["update", "--extensions"])).resolves.toBe(true);
		expect(process.exitCode).toBe(1);
	});

	it("leaves legacy package settings and directories inert", async () => {
		const tempDir = join(tmpdir(), `bone-legacy-state-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		const agentDir = join(tempDir, "agent");
		const cwd = join(tempDir, "project");
		const globalSettingsPath = join(agentDir, "settings.json");
		const projectSettingsPath = join(cwd, ".bone", "settings.json");
		const userPackageMarker = join(agentDir, "npm", "legacy-package", "marker.txt");
		const projectPackageMarker = join(cwd, ".bone", "npm", "legacy-package", "marker.txt");

		try {
			mkdirSync(join(agentDir, "npm", "legacy-package"), { recursive: true });
			mkdirSync(join(cwd, ".bone", "npm", "legacy-package"), { recursive: true });
			writeFileSync(userPackageMarker, "keep");
			writeFileSync(projectPackageMarker, "keep");
			writeFileSync(globalSettingsPath, JSON.stringify({ packages: ["npm:legacy"], extensions: ["./legacy.ts"] }));
			writeFileSync(
				projectSettingsPath,
				JSON.stringify({ packages: ["npm:project"], extensions: ["./project.ts"] }),
			);

			const loader = new DefaultResourceLoader({ cwd, agentDir });
			await loader.reload();

			expect(loader.getExtensions().extensions).toEqual([]);
			expect(readFileSync(globalSettingsPath, "utf-8")).toBe(
				JSON.stringify({ packages: ["npm:legacy"], extensions: ["./legacy.ts"] }),
			);
			expect(readFileSync(projectSettingsPath, "utf-8")).toBe(
				JSON.stringify({ packages: ["npm:project"], extensions: ["./project.ts"] }),
			);
			expect(existsSync(userPackageMarker)).toBe(true);
			expect(existsSync(projectPackageMarker)).toBe(true);
		} finally {
			rmSync(tempDir, { recursive: true, force: true });
		}
	});
});
