import { EventEmitter } from "node:events";
import type { FSWatcher, WatchListener } from "node:fs";
import { describe, expect, it, vi } from "vitest";
import { watchWithErrorHandler } from "../../../src/utils/fs-watch.ts";

describe("issue #2791 fs.watch error handling", () => {
	it("attaches an error listener and delegates watcher failures", () => {
		const watcher = new EventEmitter() as FSWatcher;
		watcher.close = vi.fn();
		const onError = vi.fn();
		const watchFile = vi.fn((_path: string, _listener: WatchListener<string>) => watcher);

		const result = watchWithErrorHandler("/tmp/theme", vi.fn(), onError, watchFile);
		expect(result).toBe(watcher);
		expect(watcher.listenerCount("error")).toBe(1);

		expect(() => watcher.emit("error", new Error("simulated watcher failure"))).not.toThrow();
		expect(onError).toHaveBeenCalledOnce();
	});

	it("reports synchronous watch failures without throwing", () => {
		const onError = vi.fn();
		const watchFile = vi.fn((): FSWatcher => {
			throw new Error("watch unavailable");
		});

		expect(watchWithErrorHandler("/tmp/theme", vi.fn(), onError, watchFile)).toBeNull();
		expect(onError).toHaveBeenCalledOnce();
	});
});
