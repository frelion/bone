import { setKeybindings, visibleWidth } from "@frelion/bone-tui";
import { beforeAll, beforeEach, describe, expect, it } from "vitest";
import { KeybindingsManager } from "../src/core/keybindings.ts";
import { WorkspaceStatusTray } from "../src/modes/interactive/components/workspace-status-tray.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

function stripAnsi(text: string): string {
	return text.replace(/\u001b\[[0-9;]*m/g, "");
}

beforeAll(() => {
	initTheme("dark");
});

beforeEach(() => {
	setKeybindings(new KeybindingsManager());
});

describe("WorkspaceStatusTray", () => {
	it("renders a compact, non-modal workspace summary below the composer", () => {
		const tray = new WorkspaceStatusTray();
		tray.setVisible(true);
		tray.setSnapshot({
			search: { label: "Ready", detail: "All 10 exchanges indexed", tone: "success" },
			sessions: { current: "idle", background: "none", stored: 3 },
			runtime: { label: "Local CPU · GGUF mmap" },
		});

		const output = tray.render(88).map(stripAnsi);
		const text = output.join("\n");
		expect(text).toContain("Workspace status · escape close");
		expect(text).toContain("Search & memory");
		expect(text).toContain("All 10 exchanges indexed");
		expect(text).toContain("Current idle · background none · stored 3");
		expect(text).toContain("Local CPU · GGUF mmap");
		expect(output.every((line) => visibleWidth(line) <= 88)).toBe(true);
	});

	it("collapses to search health without overflowing a narrow terminal", () => {
		const tray = new WorkspaceStatusTray();
		tray.setVisible(true);
		tray.setSnapshot({
			search: { label: "Keyword ready", detail: "Semantic search needs setup · Run bone setup", tone: "warning" },
			sessions: { current: "working", background: "1 running", stored: 20 },
			runtime: { label: "Local CPU · GGUF mmap" },
		});

		const output = tray.render(40).map(stripAnsi);
		expect(output).toHaveLength(2);
		expect(output.join("\n")).toContain("Workspace status");
		expect(output.join("\n")).toContain("Search keyword ready");
		expect(output.every((line) => visibleWidth(line) <= 40)).toBe(true);
	});

	it("does not render while hidden", () => {
		expect(new WorkspaceStatusTray().render(80)).toEqual([]);
	});
});
