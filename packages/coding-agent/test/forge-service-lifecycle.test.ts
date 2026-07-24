import { Agent, MockAgent } from "undici-client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createForgeService } from "../src/core/forge/service.ts";

afterEach(() => {
	vi.restoreAllMocks();
});

describe("Forge service dispatcher lifecycle", () => {
	it("closes an owned dispatcher exactly once", async () => {
		const close = vi.spyOn(Agent.prototype, "close").mockResolvedValue();
		const service = createForgeService({ cwd: process.cwd() });

		await service.close?.();
		await service.close?.();

		expect(close).toHaveBeenCalledOnce();
	});

	it("does not close a caller-owned dispatcher", async () => {
		const dispatcher = new MockAgent();
		const close = vi.spyOn(dispatcher, "close");
		const service = createForgeService({ cwd: process.cwd(), dispatcher });

		await service.close?.();

		expect(close).not.toHaveBeenCalled();
		close.mockRestore();
		await dispatcher.close();
	});
});
