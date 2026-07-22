import type { AutocompleteProvider, BoneKeyEvent, BoneTestRenderer } from "@frelion/bone-tui";
import { createBoneTestRenderer } from "@frelion/bone-tui";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { OpenTUIComposer } from "../src/modes/interactive/components/opentui-composer.ts";
import { initTheme, theme } from "../src/modes/interactive/theme/theme.ts";

const renderers = new Set<BoneTestRenderer>();

async function mountComposer(options: ConstructorParameters<typeof OpenTUIComposer>[0] = {}) {
	const renderer = await createBoneTestRenderer({ width: 60, height: 16 });
	renderers.add(renderer);
	renderer.start();
	const composer = new OpenTUIComposer(options);
	renderer.mount(composer);
	composer.focus();
	const unsubscribe = renderer.onKey((event: BoneKeyEvent) => composer.handleKey(event));
	return { renderer, composer, unsubscribe };
}

async function settle(renderer: BoneTestRenderer): Promise<void> {
	await Promise.resolve();
	await renderer.flush();
	await Promise.resolve();
}

beforeEach(() => {
	initTheme("dark");
});

afterEach(() => {
	for (const renderer of renderers) renderer.destroy();
	renderers.clear();
});

describe("OpenTUI composer", () => {
	test("types, pastes multiline text, submits, and resets", async () => {
		const changes: string[] = [];
		const submitted = vi.fn();
		const { renderer, composer } = await mountComposer({
			onChange: (value) => changes.push(value),
			onSubmit: submitted,
		});

		await renderer.input.typeText("hello");
		await renderer.input.paste("\nworld");
		expect(composer.value).toBe("hello\nworld");
		expect(changes.at(-1)).toBe("hello\nworld");

		renderer.input.pressEnter();
		await settle(renderer);
		expect(submitted).toHaveBeenCalledWith("hello\nworld");
		expect(composer.value).toBe("");
		expect(changes.at(-1)).toBe("");
	});

	test("inserts newlines with fixed structured actions without submitting", async () => {
		const submitted = vi.fn();
		const { renderer, composer } = await mountComposer({ onSubmit: submitted });
		await renderer.input.typeText("first");
		renderer.input.pressEnter({ shift: true });
		await renderer.input.typeText("second");
		renderer.input.pressKey("j", { ctrl: true });
		await renderer.input.typeText("third");

		expect(composer.value).toBe("first\nsecond\nthird");
		expect(submitted).not.toHaveBeenCalled();
	});

	test("navigates newest-first history and restores the captured draft", async () => {
		const { renderer, composer } = await mountComposer({ history: ["newest", "older"] });
		await renderer.input.typeText("draft");

		renderer.input.pressArrow("up");
		expect(composer.value).toBe("newest");
		renderer.input.pressArrow("up");
		expect(composer.value).toBe("older");
		renderer.input.pressArrow("down");
		expect(composer.value).toBe("newest");
		renderer.input.pressArrow("down");
		expect(composer.value).toBe("draft");
	});

	test("closes autocomplete before invoking cancel", async () => {
		const cancelled = vi.fn();
		const provider: AutocompleteProvider = {
			async getSuggestions() {
				return { prefix: "/", items: [{ value: "help", label: "help" }] };
			},
			applyCompletion(lines) {
				return { lines, cursorLine: 0, cursorCol: 0 };
			},
		};
		const { renderer, composer } = await mountComposer({ autocompleteProvider: provider, onCancel: cancelled });
		renderer.input.pressKey("\t");
		await settle(renderer);
		expect(composer.autocompleteOpen).toBe(true);

		renderer.input.pressEscape();
		expect(composer.autocompleteOpen).toBe(false);
		expect(cancelled).not.toHaveBeenCalled();
		renderer.input.pressEscape();
		expect(cancelled).toHaveBeenCalledOnce();
	});

	test("selects and applies a structured autocomplete item", async () => {
		const applyCompletion = vi.fn<AutocompleteProvider["applyCompletion"]>((lines, line, col, item) => ({
			lines: [`${lines[line]?.slice(0, col) ?? ""}${item.value}`],
			cursorLine: 0,
			cursorCol: col + item.value.length,
		}));
		const provider: AutocompleteProvider = {
			async getSuggestions() {
				return {
					prefix: "/",
					items: [
						{ value: "help", label: "help" },
						{ value: "history", label: "history", description: "Show history" },
					],
				};
			},
			applyCompletion,
		};
		const changes: string[] = [];
		const { renderer, composer } = await mountComposer({
			autocompleteProvider: provider,
			onChange: (value) => changes.push(value),
		});
		await renderer.input.typeText("/");
		await settle(renderer);
		expect(composer.autocompleteOpen).toBe(true);

		renderer.input.pressArrow("down");
		expect(composer.selectedAutocompleteItem?.value).toBe("history");
		renderer.input.pressEnter();
		expect(composer.value).toBe("/history");
		expect(composer.autocompleteOpen).toBe(false);
		expect(changes.at(-1)).toBe("/history");
		expect(applyCompletion).toHaveBeenCalledOnce();
	});

	test("ignores stale asynchronous autocomplete results", async () => {
		let resolveFirst: ((value: Awaited<ReturnType<AutocompleteProvider["getSuggestions"]>>) => void) | undefined;
		const first = new Promise<Awaited<ReturnType<AutocompleteProvider["getSuggestions"]>>>((resolve) => {
			resolveFirst = resolve;
		});
		let calls = 0;
		const provider: AutocompleteProvider = {
			getSuggestions() {
				calls++;
				if (calls === 1) return first;
				return Promise.resolve({ prefix: "b", items: [{ value: "beta", label: "beta" }] });
			},
			applyCompletion(lines) {
				return { lines, cursorLine: 0, cursorCol: 0 };
			},
		};
		const { renderer, composer } = await mountComposer({ autocompleteProvider: provider });
		await renderer.input.typeText("a");
		await renderer.input.typeText("b");
		await settle(renderer);
		expect(composer.selectedAutocompleteItem?.value).toBe("beta");

		resolveFirst?.({ prefix: "a", items: [{ value: "alpha", label: "alpha" }] });
		await settle(renderer);
		expect(composer.selectedAutocompleteItem?.value).toBe("beta");
	});

	test("updates placeholder and theme without replacing composer state", async () => {
		const { renderer, composer } = await mountComposer({ placeholder: "Initial prompt" });
		await settle(renderer);
		expect(renderer.captureFrame()).toContain("Initial prompt");
		composer.setPlaceholder("Updated prompt");
		initTheme("light");
		composer.updateTheme(theme);
		await settle(renderer);

		expect(renderer.captureFrame()).toContain("Updated prompt");
		expect(composer.value).toBe("");
	});

	test("keeps public state updates safe after the native edit buffer is destroyed", async () => {
		const { renderer, composer } = await mountComposer();
		await renderer.input.typeText("draft survives teardown");
		renderer.destroy();

		expect(composer.value).toBe("draft survives teardown");
		expect(() => composer.focus()).not.toThrow();
		expect(() => composer.blur()).not.toThrow();
		expect(() => composer.setPlaceholder("Next prompt")).not.toThrow();
		initTheme("light");
		expect(() => composer.updateTheme(theme)).not.toThrow();
		expect(() => composer.setAutocompleteProvider(undefined)).not.toThrow();
		expect(composer.selectedAutocompleteItem).toBeUndefined();
		expect(() => composer.setValue("updated after teardown")).not.toThrow();
		expect(composer.value).toBe("updated after teardown");
		expect(() => composer.destroy()).not.toThrow();
	});
});
