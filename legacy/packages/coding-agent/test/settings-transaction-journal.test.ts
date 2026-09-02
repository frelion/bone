import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { SettingsTransactionJournal } from "../src/core/settings-transaction-journal.ts";

describe("SettingsTransactionJournal", () => {
	const directories: string[] = [];

	afterEach(() => {
		for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
	});

	it("recovers every target to its before-image after an interrupted multi-file save", () => {
		const agentDir = createDirectory(directories);
		const projectDir = join(agentDir, "workspace", ".bone");
		const global = join(agentDir, "settings.json");
		const project = join(projectDir, "settings.json");
		const models = join(agentDir, "models.json");
		mkdirSync(projectDir, { recursive: true });
		writeFileSync(global, '{"theme":"dark"}\n', { mode: 0o600 });
		writeFileSync(project, '{"theme":"light"}\n', { mode: 0o600 });
		writeFileSync(models, '{"providers":{}}\n', { mode: 0o600 });

		const journal = SettingsTransactionJournal.begin(agentDir, [global, project, models]);
		journal.markApplying();
		writeFileSync(global, '{"theme":"changed"}\n');
		writeFileSync(project, '{"theme":"changed"}\n');
		writeFileSync(models, '{"providers":{"new":{}}}\n');

		expect(SettingsTransactionJournal.recover(agentDir)).toBe(true);
		expect(readFileSync(global, "utf8")).toBe('{"theme":"dark"}\n');
		expect(readFileSync(project, "utf8")).toBe('{"theme":"light"}\n');
		expect(readFileSync(models, "utf8")).toBe('{"providers":{}}\n');
		expect(existsSync(join(agentDir, ".bone-settings-transaction.json"))).toBe(false);
		expect(statSync(global).mode & 0o777).toBe(0o600);
	});

	it("does not recover a successfully committed transaction", () => {
		const agentDir = createDirectory(directories);
		const settings = join(agentDir, "settings.json");
		writeFileSync(settings, "old\n", { mode: 0o600 });
		const journal = SettingsTransactionJournal.begin(agentDir, [settings]);
		journal.markApplying();
		writeFileSync(settings, "new\n", { mode: 0o600 });
		journal.commit();

		expect(SettingsTransactionJournal.recover(agentDir)).toBe(false);
		expect(readFileSync(settings, "utf8")).toBe("new\n");
	});
});

function createDirectory(directories: string[]): string {
	const directory = join(tmpdir(), `bone-settings-journal-${Date.now()}-${Math.random().toString(36).slice(2)}`);
	directories.push(directory);
	mkdirSync(directory, { recursive: true });
	return directory;
}
