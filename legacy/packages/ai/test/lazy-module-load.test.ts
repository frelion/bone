import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SDK_MARKERS = ["@anthropic-ai/sdk", "openai/resources", "@google/genai", "@mistralai/mistralai"];

type BuildOutput = { path: string; text(): Promise<string> };
type BunBuild = (options: {
	entrypoints: string[];
	target: "bun";
	splitting: boolean;
	write: false;
}) => Promise<{ success: boolean; logs: unknown[]; outputs: BuildOutput[] }>;

const bunBuild = (globalThis as unknown as { Bun: { build: BunBuild } }).Bun.build;

async function buildEntry(relativeEntry: string): Promise<{ entry: string; chunks: string[] }> {
	const result = await bunBuild({
		entrypoints: [resolve(packageRoot, relativeEntry)],
		target: "bun",
		splitting: true,
		write: false,
	});
	expect(result.success, JSON.stringify(result.logs)).toBe(true);
	const outputs = await Promise.all(
		result.outputs.map(async (output) => ({ path: output.path, text: await output.text() })),
	);
	const entryName = `${relativeEntry.split("/").at(-1)?.replace(/\.ts$/, "")}.js`;
	const entry = outputs.find((output) => output.path.endsWith(entryName));
	if (!entry) throw new Error(`Missing Bun build entry output for ${relativeEntry}`);
	return { entry: entry.text, chunks: outputs.filter((output) => output !== entry).map((output) => output.text) };
}

describe("lazy provider module loading", () => {
	it("keeps provider SDKs out of the root entry", async () => {
		const output = await buildEntry("src/index.ts");
		for (const marker of SDK_MARKERS) expect(output.entry).not.toContain(marker);
	});

	it("keeps provider SDKs in split chunks for the compatibility entry", async () => {
		const output = await buildEntry("src/compat.ts");
		for (const marker of SDK_MARKERS) expect(output.entry).not.toContain(marker);
		expect(output.chunks.some((chunk) => chunk.includes("@anthropic-ai/sdk"))).toBe(true);
		expect(output.entry).toContain("import(");
	});

	it("builds the builtin provider catalog without eagerly embedding SDKs", async () => {
		const output = await buildEntry("src/providers/all.ts");
		for (const marker of SDK_MARKERS) expect(output.entry).not.toContain(marker);
	});
});
