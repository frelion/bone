import { Type } from "typebox";
import { describe, expect, it } from "vitest";
import { convertTools } from "../src/api/anthropic-messages.ts";
import type { Tool } from "../src/types.ts";

describe("Anthropic tool schema conversion", () => {
	it("preserves root unions and closed operation-specific properties", () => {
		const parameters = Type.Union([
			Type.Object(
				{ operation: Type.Literal("list"), resource: Type.Literal("issue") },
				{ additionalProperties: false },
			),
			Type.Object(
				{ operation: Type.Literal("get"), resource: Type.Literal("issue"), id: Type.Integer({ minimum: 1 }) },
				{ additionalProperties: false },
			),
		]);
		const tools: Tool[] = [{ name: "forge_query", description: "Query Forge", parameters }];

		const converted = convertTools(tools, false, false);
		const schema = converted[0]?.input_schema as unknown as {
			type?: string;
			anyOf?: Array<{ properties?: Record<string, unknown>; additionalProperties?: boolean }>;
		};

		expect(schema.type).toBe("object");
		expect(schema.anyOf).toHaveLength(2);
		expect(schema.anyOf?.[0]?.properties).toHaveProperty("operation");
		expect(schema.anyOf?.every((variant) => variant.additionalProperties === false)).toBe(true);
	});
});
