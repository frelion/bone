import { APP_NAME } from "../config.ts";
import type { SourceInfo } from "./source-info.ts";

export type SlashCommandSource = "extension" | "prompt" | "skill";

export interface SlashCommandInfo {
	name: string;
	description?: string;
	source: SlashCommandSource;
	sourceInfo: SourceInfo;
}

export interface BuiltinSlashCommand {
	name: string;
	description: string;
	argumentHint?: string;
}

export const BUILTIN_SLASH_COMMANDS: ReadonlyArray<BuiltinSlashCommand> = [
	{ name: "settings", description: "Open settings menu" },
	{ name: "model", description: "Select model (opens selector UI)", argumentHint: "<provider/model>" },
	{ name: "scoped-models", description: "Enable/disable models for Ctrl+P cycling" },
	{ name: "export", description: "Export conversation (HTML default, or specify path: .html/.jsonl)" },
	{ name: "import", description: "Import a conversation from a JSONL file" },
	{ name: "share", description: "Share conversation as a secret GitHub gist" },
	{ name: "copy", description: "Copy last agent message to clipboard" },
	{ name: "name", description: "Generate or set conversation display name", argumentHint: "[name]" },
	{ name: "conversation", description: "Show conversation info and stats" },
	{ name: "status", description: "Show Bone runtime and memory status" },
	{ name: "conversations", description: "Focus conversations in Side" },
	{ name: "changelog", description: "Show changelog entries" },
	{ name: "hotkeys", description: "Show all keyboard shortcuts" },
	{ name: "fork", description: "Create a new fork from a previous user message" },
	{ name: "clone", description: "Duplicate the current conversation at the current position" },
	{ name: "tree", description: "Navigate conversation branches" },
	{ name: "trust", description: "Save workspace trust decision" },
	{ name: "login", description: "Configure provider authentication", argumentHint: "<provider>" },
	{ name: "logout", description: "Remove provider authentication" },
	{ name: "new", description: "Start a new conversation" },
	{ name: "compact", description: "Manually compact this conversation" },
	{ name: "plan", description: "Toggle Plan mode" },
	{ name: "reload", description: "Reload keybindings, local skills, prompts, themes, and context files" },
	{ name: "quit", description: `Quit ${APP_NAME}` },
];
