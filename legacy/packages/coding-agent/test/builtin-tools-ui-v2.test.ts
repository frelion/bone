import { describe, expect, it } from "vitest";
import { createBashToolDefinition } from "../src/core/tools/bash.ts";
import { createEditToolDefinition } from "../src/core/tools/edit.ts";
import { createFindToolDefinition } from "../src/core/tools/find.ts";
import { createGrepToolDefinition } from "../src/core/tools/grep.ts";
import { createLsToolDefinition } from "../src/core/tools/ls.ts";
import { createReadToolDefinition } from "../src/core/tools/read.ts";
import { createWriteToolDefinition } from "../src/core/tools/write.ts";

describe("built-in tool UI v2", () => {
	it("exposes only structured renderers for every built-in tool", () => {
		const definitions = [
			createReadToolDefinition("/tmp"),
			createWriteToolDefinition("/tmp"),
			createEditToolDefinition("/tmp"),
			createBashToolDefinition("/tmp"),
			createGrepToolDefinition("/tmp"),
			createFindToolDefinition("/tmp"),
			createLsToolDefinition("/tmp"),
		];
		for (const definition of definitions) {
			expect(definition.renderV2?.renderCall, definition.name).toBeTypeOf("function");
			expect(definition.renderV2?.renderResult, definition.name).toBeTypeOf("function");
			expect("renderCall" in definition, definition.name).toBe(false);
			expect("renderResult" in definition, definition.name).toBe(false);
		}
	});
});
