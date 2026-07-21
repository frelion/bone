import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { createEventBus } from "../src/core/event-bus.ts";
import {
	createExtensionRuntime,
	discoverAndLoadExtensions,
	loadExtensionFromFactory,
	loadExtensions,
} from "../src/core/extensions/loader.ts";

describe("Bone extension boundary", () => {
	it("does not discover filesystem extensions or Pi manifests", async () => {
		const tempDir = mkdtempSync(join(tmpdir(), "bone-extension-boundary-"));
		try {
			const extensionDir = join(tempDir, "extensions");
			mkdirSync(extensionDir);
			writeFileSync(join(extensionDir, "index.ts"), "export default () => {}");
			writeFileSync(join(tempDir, "package.json"), JSON.stringify({ pi: { extensions: ["index.ts"] } }));

			const discovered = await discoverAndLoadExtensions([], tempDir, tempDir);
			expect(discovered.extensions).toEqual([]);
			expect(discovered.errors).toEqual([]);
		} finally {
			rmSync(tempDir, { recursive: true, force: true });
		}
	});

	it("rejects explicit extension paths", async () => {
		const result = await loadExtensions(["./extension.ts"], process.cwd());
		expect(result.extensions).toEqual([]);
		expect(result.errors[0]?.error).toContain("External extensions are not supported");
	});

	it("keeps inline factories available to Bone-owned modules", async () => {
		const runtime = createExtensionRuntime();
		const extension = await loadExtensionFromFactory(
			(api) => {
				api.registerCommand("bone-internal", { description: "internal", handler: async () => {} });
			},
			process.cwd(),
			createEventBus(),
			runtime,
		);
		expect(extension.commands.get("bone-internal")?.description).toBe("internal");
	});
});
