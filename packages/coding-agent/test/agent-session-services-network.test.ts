import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createAgentSessionServices } from "../src/core/agent-session-services.ts";
import { ModelRuntime } from "../src/core/model-runtime.ts";

const tempDirs: string[] = [];

afterEach(() => {
	vi.restoreAllMocks();
	for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe("createAgentSessionServices model initialization", () => {
	it("forwards the local-only network policy to new model runtimes", async () => {
		const dir = mkdtempSync(join(tmpdir(), "bone-services-network-"));
		tempDirs.push(dir);
		const runtime = {
			refresh: vi.fn().mockResolvedValue({ aborted: false, errors: new Map() }),
		} as unknown as ModelRuntime;
		const create = vi.spyOn(ModelRuntime, "create").mockResolvedValue(runtime);

		await createAgentSessionServices({
			cwd: dir,
			agentDir: dir,
			allowModelNetwork: false,
			resourceLoaderOptions: {
				noExtensions: true,
				noSkills: true,
				noPromptTemplates: true,
				noThemes: true,
				noContextFiles: true,
			},
		});

		expect(create).toHaveBeenCalledWith(
			expect.objectContaining({
				authPath: join(dir, "auth.json"),
				modelsPath: join(dir, "models.json"),
				allowModelNetwork: false,
			}),
		);
		expect(runtime.refresh).toHaveBeenCalledWith({ allowNetwork: false });
	});
});
