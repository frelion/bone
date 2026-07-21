import { setKeybindings } from "@frelion/bone-tui";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { KeybindingsManager } from "../src/core/keybindings.ts";
import { ModelTaskSelectorComponent } from "../src/modes/interactive/components/model-task-selector.ts";
import { initTheme } from "../src/modes/interactive/theme/theme.ts";

beforeAll(() => {
	initTheme("dark");
});

beforeEach(() => {
	setKeybindings(new KeybindingsManager());
});

describe("ModelTaskSelectorComponent", () => {
	it("shows conversation and title assignments and selects title generation", () => {
		const select = vi.fn();
		const selector = new ModelTaskSelectorComponent({
			conversationModel: undefined,
			titleModel: { providerId: "openai", modelId: "gpt-4.1-mini" },
			onSelect: select,
			onCancel: vi.fn(),
		});

		const output = selector.render(80).join("\n");
		expect(output).toContain("Conversation");
		expect(output).toContain("Title generation");
		expect(output).toContain("gpt-4.1-mini · openai");

		selector.handleInput("\x1b[B");
		selector.handleInput("\r");
		expect(select).toHaveBeenCalledWith("title");
	});
});
