import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { performance } from "node:perf_hooks";
import { SessionManager } from "../src/core/session-manager.ts";

function percentile(values: number[], ratio: number): number {
	const sorted = [...values].sort((a, b) => a - b);
	return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * ratio))] ?? 0;
}

const root = mkdtempSync(join(tmpdir(), "bone-session-load-benchmark-"));
try {
	const timestamp = new Date().toISOString();
	const lines = [JSON.stringify({ type: "session", version: 3, id: "benchmark", timestamp, cwd: root })];
	let parentId: string | null = null;
	for (let index = 0; index < 4_000; index++) {
		const id = `entry-${index}`;
		lines.push(
			JSON.stringify({
				type: "message",
				id,
				parentId,
				timestamp,
				message: {
					role: "user",
					content: [{ type: "text", text: `benchmark ${index} ${"x".repeat(512)}` }],
					timestamp: index,
				},
			}),
		);
		parentId = id;
	}
	const sessionPath = join(root, "benchmark.jsonl");
	writeFileSync(sessionPath, `${lines.join("\n")}\n`);

	const syncRuns: number[] = [];
	const asyncRuns: number[] = [];
	for (let run = 0; run < 8; run++) {
		let startedAt = performance.now();
		SessionManager.open(sessionPath);
		syncRuns.push(performance.now() - startedAt);
		startedAt = performance.now();
		await SessionManager.openAsync(sessionPath);
		asyncRuns.push(performance.now() - startedAt);
	}

	console.log(
		JSON.stringify(
			{
				fileBytes: Buffer.byteLength(`${lines.join("\n")}\n`),
				sync: { p50Ms: percentile(syncRuns, 0.5), p95Ms: percentile(syncRuns, 0.95) },
				cooperative: { p50Ms: percentile(asyncRuns, 0.5), p95Ms: percentile(asyncRuns, 0.95) },
			},
			null,
			2,
		),
	);
} finally {
	rmSync(root, { recursive: true, force: true });
}
