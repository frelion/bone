import type { BoneInputNode, BoneNode, BoneRenderContext, BoneView } from "@frelion/bone-tui";
import { type Theme, theme } from "../theme/theme.ts";
import { createOpenTUIDialogShell, type OpenTUIDialogMount } from "./opentui-dialog-v2.ts";
import { type OpenTUISelectorAction, type OpenTUISelectorItem, OpenTUISelectorViewV2 } from "./opentui-selector-v2.ts";

export interface OpenTUISettingItemV2 {
	id: string;
	label: string;
	value: string;
	description?: string;
	disabled?: boolean;
}

export interface OpenTUISettingsListOptionsV2 {
	title: string;
	items: readonly OpenTUISettingItemV2[];
	onActivate: (id: string) => void;
	onCancel: () => void;
	theme?: Theme;
}

export class OpenTUISettingsListViewV2 implements BoneView {
	private readonly selector: OpenTUISelectorViewV2<string>;

	constructor(options: OpenTUISettingsListOptionsV2) {
		const items: OpenTUISelectorItem<string>[] = options.items.map((item) => ({
			value: item.id,
			label: item.label,
			description: item.value,
			keywords: item.description,
			disabled: item.disabled,
		}));
		this.selector = new OpenTUISelectorViewV2({
			title: options.title,
			items,
			theme: options.theme,
			onSelect: options.onActivate,
			onCancel: options.onCancel,
		});
	}

	mount(context: BoneRenderContext): BoneNode {
		return this.selector.mount(context);
	}

	handleAction(action: OpenTUISelectorAction): boolean {
		return this.selector.handleAction(action);
	}
}

export interface OpenTUIFormFieldV2 {
	id: string;
	label: string;
	value?: string;
	placeholder?: string;
	description?: string;
	required?: boolean;
}

export interface OpenTUIFormOptionsV2 {
	title: string;
	subtitle?: string;
	fields: readonly OpenTUIFormFieldV2[];
	onSubmit: (values: Readonly<Record<string, string>>) => void;
	onCancel: () => void;
	validate?: (values: Readonly<Record<string, string>>) => string | undefined;
	theme?: Theme;
}

/** Structured text form primitive with product-owned submit validation. */
export class OpenTUIFormViewV2 implements BoneView {
	private readonly options: OpenTUIFormOptionsV2;
	private readonly formTheme: Theme;
	private readonly values = new Map<string, string>();
	private readonly inputs = new Map<string, BoneInputNode>();
	private dialog: OpenTUIDialogMount | undefined;
	private selectedField = 0;

	constructor(options: OpenTUIFormOptionsV2) {
		this.options = options;
		this.formTheme = options.theme ?? theme;
		for (const field of options.fields) this.values.set(field.id, field.value ?? "");
	}

	mount(context: BoneRenderContext): BoneNode {
		this.dialog = createOpenTUIDialogShell(context, {
			title: this.options.title,
			subtitle: this.options.subtitle,
			footer: "↑↓ fields · Enter save · Esc cancel",
			theme: this.formTheme,
		});
		for (const [index, field] of this.options.fields.entries()) {
			this.dialog.body.append(
				context.createText({
					content: field.required ? `${field.label} *` : field.label,
					fg: this.formTheme.getFgColor("text"),
					bold: index === this.selectedField,
				}),
			);
			if (field.description) {
				this.dialog.body.append(
					context.createText({ content: field.description, fg: this.formTheme.getFgColor("muted") }),
				);
			}
			const input = context.createInput({
				width: "100%",
				value: field.value ?? "",
				placeholder: field.placeholder ?? "",
				textColor: this.formTheme.getFgColor("text"),
				focusedTextColor: this.formTheme.getFgColor("text"),
				placeholderColor: this.formTheme.getFgColor("dim"),
				onInput: (value) => this.values.set(field.id, value),
			});
			this.inputs.set(field.id, input);
			this.dialog.body.append(input);
			this.dialog.body.append(context.createSpacer({ size: 1, direction: "vertical" }));
		}
		this.focusSelectedField();
		return this.dialog.root;
	}

	handleAction(action: "confirm" | "cancel" | "up" | "down"): boolean {
		if (action === "cancel") {
			this.options.onCancel();
			return true;
		}
		if (action === "up" || action === "down") {
			const delta = action === "up" ? -1 : 1;
			this.selectedField = Math.max(0, Math.min(this.options.fields.length - 1, this.selectedField + delta));
			this.focusSelectedField();
			return true;
		}
		const values = Object.fromEntries(this.values);
		const missing = this.options.fields.find((field) => field.required && !values[field.id]?.trim());
		const error = missing ? `${missing.label} is required` : this.options.validate?.(values);
		if (error) {
			if (this.dialog) {
				this.dialog.status.content = error;
				this.dialog.status.visible = true;
			}
			return true;
		}
		this.options.onSubmit(values);
		return true;
	}

	private focusSelectedField(): void {
		const field = this.options.fields[this.selectedField];
		if (field) this.inputs.get(field.id)?.focus();
	}
}
