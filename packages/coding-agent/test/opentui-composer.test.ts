import type { AutocompleteProvider } from "@frelion/bone-tui";
import type { KeyEvent } from "@opentui/core";
import { createTestRenderer, type TestRendererSetup } from "@opentui/core/testing";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import { OpenTUIComposer } from "../src/modes/interactive/components/opentui-composer.ts";
import { initTheme, theme } from "../src/modes/interactive/theme/theme.ts";

const setups = new Set<TestRendererSetup>();

async function mountComposer(options: ConstructorParameters<typeof OpenTUIComposer>[0] = {}) {
	const testSetup = await createTestRenderer({
		width: 60,
		height: 16,
		autoFocus: false,
		useMouse: true,
		kittyKeyboard: true,
	});
	setups.add(testSetup);
	const { renderer } = testSetup;
	renderer.start();
	const composer = new OpenTUIComposer(renderer, options);
	renderer.root.add(composer.root);
	composer.focus();
	const onKey = (event: KeyEvent) => composer.handleKey(event);
	renderer.keyInput.prependListener("keypress", onKey);
	const unsubscribe = () => renderer.keyInput.off("keypress", onKey);
	return { testSetup, renderer, mockInput: testSetup.mockInput, composer, unsubscribe };
}

async function settle(setup: TestRendererSetup): Promise<void> {
	await Promise.resolve();
	await setup.flush();
	await Promise.resolve();
}

beforeEach(() => {
	initTheme("dark");
});

afterEach(() => {
	for (const setup of setups) setup.renderer.destroy();
	setups.clear();
});

describe("OpenTUI composer", () => {
	test("types, pastes multiline text, submits, and resets", async () => {
		const changes: string[] = [];
		const submitted = vi.fn();
		const { testSetup, mockInput, composer } = await mountComposer({
			onChange: (value) => changes.push(value),
			onSubmit: submitted,
		});

		await mockInput.typeText("hello");
		await mockInput.pasteBracketedText("\nworld");
		expect(composer.value).toBe("hello\nworld");
		expect(changes.at(-1)).toBe("hello\nworld");

		mockInput.pressEnter();
		await settle(testSetup);
		expect(submitted).toHaveBeenCalledWith("hello\nworld");
		expect(composer.value).toBe("");
		expect(changes.at(-1)).toBe("");
	});

	test("inserts newlines with fixed structured actions without submitting", async () => {
		const submitted = vi.fn();
		const { mockInput, composer } = await mountComposer({ onSubmit: submitted });
		await mockInput.typeText("first");
		mockInput.pressEnter({ shift: true });
		await mockInput.typeText("second");
		mockInput.pressKey("j", { ctrl: true });
		await mockInput.typeText("third");

		expect(composer.value).toBe("first\nsecond\nthird");
		expect(submitted).not.toHaveBeenCalled();
	});

	test("navigates newest-first history and restores the captured draft", async () => {
		const { mockInput, composer } = await mountComposer({ history: ["newest", "older"] });
		await mockInput.typeText("draft");

		mockInput.pressArrow("up");
		expect(composer.value).toBe("newest");
		mockInput.pressArrow("up");
		expect(composer.value).toBe("older");
		mockInput.pressArrow("down");
		expect(composer.value).toBe("newest");
		mockInput.pressArrow("down");
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
		const { testSetup, mockInput, composer } = await mountComposer({
			autocompleteProvider: provider,
			onCancel: cancelled,
		});
		mockInput.pressKey("\t");
		await settle(testSetup);
		expect(composer.autocompleteOpen).toBe(true);

		mockInput.pressEscape();
		await settle(testSetup);
		expect(composer.autocompleteOpen).toBe(false);
		expect(cancelled).not.toHaveBeenCalled();
		mockInput.pressEscape();
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
		const submitted = vi.fn();
		const { testSetup, mockInput, composer } = await mountComposer({
			autocompleteProvider: provider,
			onChange: (value) => changes.push(value),
			onSubmit: submitted,
		});
		await mockInput.typeText("/");
		await settle(testSetup);
		expect(composer.autocompleteOpen).toBe(true);

		mockInput.pressArrow("down");
		expect(composer.selectedAutocompleteItem?.value).toBe("history");
		mockInput.pressEnter();
		await settle(testSetup);
		expect(composer.value).toBe("/history");
		expect(composer.autocompleteOpen).toBe(false);
		expect(changes.at(-1)).toBe("/history");
		expect(applyCompletion).toHaveBeenCalledOnce();
		mockInput.pressEnter();
		expect(submitted).toHaveBeenCalledWith("/history");
	});

	test("limits focus requests to the prompt and preserves child mouse ownership", async () => {
		const provider: AutocompleteProvider = {
			async getSuggestions() {
				return { prefix: "/", items: [{ value: "help", label: "help" }] };
			},
			applyCompletion(lines) {
				return { lines, cursorLine: 0, cursorCol: 0 };
			},
		};
		const { testSetup, mockInput, composer } = await mountComposer({ autocompleteProvider: provider });
		const focusRequest = vi.fn();
		composer.onFocusRequest = focusRequest;
		composer.setQueuedMessages([{ id: "queued", text: "Run focused tests" }]);
		await mockInput.typeText("/");
		await settle(testSetup);
		expect(composer.autocompleteOpen).toBe(true);

		await testSetup.mockMouse.click(composer.autocomplete.screenX + 1, composer.autocomplete.screenY);
		const queuePanel = composer.root.getChildren()[1];
		if (!queuePanel) throw new Error("Expected queue panel");
		await testSetup.mockMouse.click(queuePanel.screenX + 1, queuePanel.screenY);
		expect(focusRequest).not.toHaveBeenCalled();

		await testSetup.mockMouse.click(composer.input.screenX + 1, composer.input.screenY);
		expect(focusRequest).toHaveBeenCalledOnce();
	});

	test("submits a unique slash-command match with one Enter press", async () => {
		const provider: AutocompleteProvider = {
			async getSuggestions() {
				return { prefix: "/set", items: [{ value: "settings", label: "settings" }] };
			},
			applyCompletion() {
				return { lines: ["/settings "], cursorLine: 0, cursorCol: 10 };
			},
		};
		const submitted = vi.fn();
		const { testSetup, mockInput } = await mountComposer({ autocompleteProvider: provider, onSubmit: submitted });

		await mockInput.typeText("/set");
		await settle(testSetup);
		mockInput.pressEnter();

		expect(submitted).toHaveBeenCalledWith("/settings");
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
		const { testSetup, mockInput, composer } = await mountComposer({ autocompleteProvider: provider });
		await mockInput.typeText("a");
		await mockInput.typeText("b");
		await settle(testSetup);
		expect(composer.selectedAutocompleteItem?.value).toBe("beta");

		resolveFirst?.({ prefix: "a", items: [{ value: "alpha", label: "alpha" }] });
		await settle(testSetup);
		expect(composer.selectedAutocompleteItem?.value).toBe("beta");
	});

	test("updates placeholder and theme without replacing composer state", async () => {
		const { testSetup, composer } = await mountComposer({ placeholder: "Initial prompt" });
		await settle(testSetup);
		expect(testSetup.captureCharFrame()).toContain("Initial prompt");
		composer.setPlaceholder("Updated prompt");
		initTheme("light");
		composer.updateTheme(theme);
		await settle(testSetup);

		expect(testSetup.captureCharFrame()).toContain("Updated prompt");
		expect(composer.value).toBe("");
	});

	test("renders a bordered prompt with one fixed status row and no legacy prompt chrome", async () => {
		const { testSetup, composer } = await mountComposer({
			status: {
				cwd: "~/src/bone",
				model: "openai/gpt-5",
				thinking: "high",
				contextRemaining: "82%",
				foregroundThroughput: "14.2 tok/s",
			},
		});
		await settle(testSetup);
		const initial = testSetup.captureCharFrame();
		expect(initial).toContain("Ask anything");
		expect(initial).toContain("~/src/bone  openai/gpt-5  high");
		expect(initial).toContain("82% left  14.2 tok/s");
		expect(initial).not.toContain("Message Bone");
		expect(initial).not.toContain("›");

		const occupiedRows = initial.split("\n").filter((line) => line.trim()).length;
		composer.updateStatus({ contextRemaining: "79%", foregroundThroughput: "31.8 tok/s" });
		await settle(testSetup);
		const streaming = testSetup.captureCharFrame();
		expect(streaming).toContain("79% left  31.8 tok/s");
		expect(streaming.split("\n").filter((line) => line.trim())).toHaveLength(occupiedRows);
	});

	test("presents task-aware placeholder and action hints", async () => {
		const { testSetup, composer } = await mountComposer({ placeholder: "Start a task" });
		await settle(testSetup);
		expect(testSetup.captureCharFrame()).toContain("Start a task");

		composer.setInteractionState({ kind: "working" });
		await settle(testSetup);
		const working = testSetup.captureCharFrame();
		expect(composer.interactionKind).toBe("working");
		expect(working).toContain("Add instructions to the current task");
		expect(working).toContain("Enter add to current task");
		expect(working).toContain("Ctrl+C stop");

		composer.setInteractionState({
			kind: "waiting",
			placeholder: "Answer the permission question",
			leftHint: "Enter confirm",
		});
		await settle(testSetup);
		const waiting = testSetup.captureCharFrame();
		expect(waiting).toContain("Answer the permission question");
		expect(waiting).toContain("Enter confirm");
		expect(waiting).toContain("Esc cancel");

		composer.setInteractionState({ kind: "idle" });
		await settle(testSetup);
		expect(testSetup.captureCharFrame()).toContain("Start a task");
	});

	test("renders a read-only snapshot of queued messages", async () => {
		const { testSetup, composer } = await mountComposer();
		composer.setQueuedMessages([
			{ id: "first", text: "Run the focused tests" },
			{ id: "second", text: "Then update\nrelease notes" },
		]);
		await settle(testSetup);

		const queued = testSetup.captureCharFrame();
		expect(composer.queuedMessageCount).toBe(2);
		expect(queued).toContain("Queued next: 2");
		expect(queued).toContain("1. Run the focused tests");
		expect(queued).toContain("2. Then update release notes");

		expect(composer.getQueuedMessages()).toEqual([
			{ id: "first", text: "Run the focused tests" },
			{ id: "second", text: "Then update\nrelease notes" },
		]);
		composer.setQueuedMessages([]);
		expect(composer.queuedMessageCount).toBe(0);
		await settle(testSetup);
		expect(testSetup.captureCharFrame()).not.toContain("Queued next");
	});

	test("keeps public state updates safe after the native edit buffer is destroyed", async () => {
		const { renderer, mockInput, composer } = await mountComposer();
		await mockInput.typeText("draft survives teardown");
		renderer.destroy();

		expect(composer.value).toBe("draft survives teardown");
		expect(() => composer.focus()).not.toThrow();
		expect(() => composer.blur()).not.toThrow();
		expect(() => composer.setPlaceholder("Next prompt")).not.toThrow();
		expect(() => composer.setInteractionState({ kind: "working" })).not.toThrow();
		expect(() => composer.setQueuedMessages([{ id: "queued", text: "queued task" }])).not.toThrow();
		expect(composer.getQueuedMessages()).toEqual([{ id: "queued", text: "queued task" }]);
		expect(() => composer.updateStatus({ foregroundThroughput: "20 tok/s" })).not.toThrow();
		initTheme("light");
		expect(() => composer.updateTheme(theme)).not.toThrow();
		expect(() => composer.setAutocompleteProvider(undefined)).not.toThrow();
		expect(composer.selectedAutocompleteItem).toBeUndefined();
		expect(() => composer.setValue("updated after teardown")).not.toThrow();
		expect(composer.value).toBe("updated after teardown");
		expect(() => composer.destroy()).not.toThrow();
	});
});
