import { type BoneTestRenderer, createBoneTestRenderer } from "@frelion/bone-tui";
import { mkdtempSync, rmSync } from "fs";
import { tmpdir } from "os";
import { join } from "path";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { SettingsManager } from "../src/core/settings-manager.ts";
import { OpenTUIConfigSelectorV2 } from "../src/modes/interactive/components/opentui-config-selector.ts";
import { OpenTUIFirstTimeSetupV2 } from "../src/modes/interactive/components/opentui-first-time-setup.ts";
import { OpenTUISessionPickerV2 } from "../src/modes/interactive/components/opentui-session-picker.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<BoneTestRenderer>();
const temporaryDirectories: string[] = [];

async function renderer(): Promise<BoneTestRenderer> {
	const value = await createBoneTestRenderer({ width: 90, height: 26 });
	renderers.add(value);
	value.start();
	return value;
}

async function frameWith(value: BoneTestRenderer, text: string): Promise<string> {
	for (let attempt = 0; attempt < 10; attempt++) {
		await value.flush();
		const frame = value.captureFrame();
		if (frame.includes(text)) return frame;
		await Promise.resolve();
	}
	return value.captureFrame();
}

function temporaryDirectory(): string {
	const path = mkdtempSync(join(tmpdir(), "bone-opentui-startup-"));
	temporaryDirectories.push(path);
	return path;
}

beforeEach(() => initTheme("dark"));

afterEach(() => {
	for (const value of renderers) value.destroy();
	renderers.clear();
	for (const path of temporaryDirectories.splice(0)) rmSync(path, { recursive: true, force: true });
});

describe("OpenTUI startup views", () => {
	test("completes theme and analytics setup as a two-step structured flow", async () => {
		const value = await renderer();
		const submit = vi.fn();
		const setup = new OpenTUIFirstTimeSetupV2({ detectedTheme: "dark", onSubmit: submit, onCancel: vi.fn() });
		value.mount(setup);
		expect(await frameWith(value, "Pick a theme")).toContain("Dark");
		setup.handleAction("down");
		setup.handleAction("confirm");
		expect(await frameWith(value, "Anonymous usage data")).toContain("Don't share");
		setup.handleAction("down");
		setup.handleAction("confirm");
		expect(submit).toHaveBeenCalledWith({ theme: "light", shareAnalytics: false });
	});

	test("loads, searches, switches scope, and selects conversations", async () => {
		const value = await renderer();
		const selected = vi.fn();
		const session = (path: string, name: string, cwd: string) => ({
			path,
			id: path,
			cwd,
			name,
			created: new Date(),
			modified: new Date(),
			messageCount: 2,
			firstMessage: name,
			allMessagesText: name,
		});
		const picker = new OpenTUISessionPickerV2({
			currentSessionsLoader: async () => [session("/sessions/current.jsonl", "Current task", "/repo")],
			allSessionsLoader: async () => [session("/sessions/other.jsonl", "Other task", "/other")],
			onSelect: selected,
			onCancel: vi.fn(),
			onExit: vi.fn(),
		});
		value.mount(picker);
		expect(await frameWith(value, "Current task")).toContain("Current folder");
		picker.handleAction("confirm");
		expect(selected).toHaveBeenCalledWith("/sessions/current.jsonl");
		picker.handleCommand("scope");
		expect(await frameWith(value, "Other task")).toContain("/other");
	});

	test("toggles global resources and project overrides", async () => {
		const value = await renderer();
		const cwd = temporaryDirectory();
		const agentDir = temporaryDirectory();
		const manager = SettingsManager.create(cwd, agentDir);
		const skillPath = join(agentDir, "skills", "review", "SKILL.md");
		const resource = {
			path: skillPath,
			enabled: true,
			metadata: { source: "auto", scope: "user" as const, origin: "top-level" as const, baseDir: agentDir },
		};
		const empty = { prompts: [], themes: [] };
		const selector = new OpenTUIConfigSelectorV2({
			resolvedPaths: {
				global: { skills: [resource], ...empty },
				project: { skills: [resource], ...empty },
			},
			settingsManager: manager,
			cwd,
			agentDir,
			writeScope: "global",
			projectModeAvailable: true,
			onClose: vi.fn(),
			onExit: vi.fn(),
		});
		value.mount(selector);
		expect(await frameWith(value, "review")).toContain("[x]");
		selector.handleCommand("toggle");
		expect(manager.getGlobalSettings().skills).toContain("-skills/review/SKILL.md");
		selector.handleCommand("scope");
		selector.handleCommand("toggle");
		expect(manager.getProjectSettings().skills).toContain(`-${skillPath}`);
	});
});
