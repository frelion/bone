import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { ModelConfig } from "../src/core/model-config.ts";

describe("ModelConfig settings transaction support", () => {
	const tempDirs: string[] = [];

	afterEach(() => {
		for (const directory of tempDirs.splice(0)) {
			if (existsSync(directory)) rmSync(directory, { recursive: true, force: true });
		}
	});

	it("exports an isolated snapshot and atomically saves schema-valid provider and model definitions", async () => {
		const directory = join(tmpdir(), `bone-model-config-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(directory);
		mkdirSync(directory, { recursive: true });
		const path = join(directory, "models.json");
		const original = {
			providers: {
				ikuncode: {
					name: "IkunCode",
					baseUrl: "https://api.example.test/v1",
					api: "openai-completions",
					models: [{ id: "luna", contextWindow: 128000, maxTokens: 8192 }],
				},
			},
		};

		ModelConfig.save(path, original);
		const config = await ModelConfig.load(path);
		const draft = config.toJson();
		draft.providers.ikuncode!.models!.push({ id: "nova", reasoning: true, input: ["text", "image"] });
		ModelConfig.save(path, draft);

		expect(JSON.parse(readFileSync(path, "utf8"))).toEqual(draft);
		expect(config.toJson().providers.ikuncode!.models).toHaveLength(1);
	});

	it("rejects invalid model documents before replacing the existing file", () => {
		const directory = join(tmpdir(), `bone-model-config-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		tempDirs.push(directory);
		mkdirSync(directory, { recursive: true });
		const path = join(directory, "models.json");
		const valid = { providers: { ikuncode: { models: [{ id: "luna" }] } } };
		ModelConfig.save(path, valid);
		const before = readFileSync(path, "utf8");

		expect(() => ModelConfig.save(path, { providers: { ikuncode: { models: [{ id: "" }] } } })).toThrow(
			"Invalid models configuration",
		);
		expect(readFileSync(path, "utf8")).toBe(before);
	});
});
