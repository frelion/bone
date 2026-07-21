import { Markdown, type MarkdownTheme } from "@frelion/bone-tui";
import chalk from "chalk";
import {
	APP_NAME,
	detectInstallMethod,
	getPackageDir,
	getSelfUpdateCommand,
	getSelfUpdateUnavailableInstruction,
	PACKAGE_NAME,
	type SelfUpdateCommand,
	type SelfUpdatePackageTarget,
	VERSION,
} from "./config.ts";
import { spawnProcess } from "./utils/child-process.ts";
import { getLatestBoneRelease, isNewerPackageVersion } from "./utils/version-check.ts";
import {
	cleanupWindowsSelfUpdateQuarantine,
	quarantineWindowsNativeDependencies,
} from "./utils/windows-self-update.ts";

const SELF_UPDATE_NOTE_MARKDOWN_THEME: MarkdownTheme = {
	heading: (text) => chalk.bold(chalk.yellow(text)),
	link: (text) => chalk.cyan(text),
	linkUrl: (text) => chalk.dim(text),
	code: (text) => chalk.yellow(text),
	codeBlock: (text) => chalk.dim(text),
	codeBlockBorder: (text) => chalk.dim(text),
	quote: (text) => chalk.dim(text),
	quoteBorder: (text) => chalk.dim(text),
	hr: (text) => chalk.dim(text),
	listBullet: (text) => chalk.yellow(text),
	bold: (text) => chalk.bold(text),
	italic: (text) => chalk.italic(text),
	strikethrough: (text) => chalk.strikethrough(text),
	underline: (text) => chalk.underline(text),
};

interface SelfUpdatePlan {
	packageName: string;
	installSpec: string;
	version: string;
	shouldRun: boolean;
	note?: string;
}

function printSelfUpdateUnavailable(
	npmCommand?: string[],
	updatePackageTarget: SelfUpdatePackageTarget = PACKAGE_NAME,
): void {
	console.error(`error: ${APP_NAME} cannot self-update this installation.`);
	console.error(getSelfUpdateUnavailableInstruction(PACKAGE_NAME, npmCommand, updatePackageTarget));
	const entrypoint = process.argv[1];
	if (entrypoint) console.error(`\nLocation of ${APP_NAME} executable: ${entrypoint}`);
}

function printSelfUpdateFallback(command: SelfUpdateCommand): void {
	console.error(chalk.dim(`If this keeps failing, run this command yourself: ${command.display}`));
}

function printPnpmSelfUpdateMetadataHint(): void {
	console.error(chalk.yellow("If pnpm reports missing package versions, its cached registry metadata may be stale."));
	console.error(chalk.yellow(`Run \`pnpm store prune\` and retry \`${APP_NAME} update\`.`));
}

function printSelfUpdateNote(note: string): void {
	const trimmedNote = note.trim();
	if (!trimmedNote) return;
	console.log();
	console.log(chalk.bold(chalk.yellow("Update note")));
	try {
		const width = Math.max(20, process.stdout.columns ?? 80);
		console.log(
			new Markdown(trimmedNote, 0, 0, SELF_UPDATE_NOTE_MARKDOWN_THEME)
				.render(width)
				.map((line) => line.trimEnd())
				.join("\n"),
		);
	} catch {
		console.log(trimmedNote);
	}
	console.log();
}

async function getSelfUpdatePlan(force: boolean): Promise<SelfUpdatePlan> {
	let latestRelease: Awaited<ReturnType<typeof getLatestBoneRelease>>;
	try {
		latestRelease = await getLatestBoneRelease(VERSION);
	} catch (error: unknown) {
		const message = error instanceof Error ? error.message : String(error);
		throw new Error(`Could not determine latest ${APP_NAME} version: ${message}`);
	}
	if (!latestRelease) throw new Error(`Could not determine latest ${APP_NAME} version.`);

	const packageName = latestRelease.packageName ?? PACKAGE_NAME;
	const installSpec = `${packageName}@${latestRelease.version}`;
	if (force || packageName !== PACKAGE_NAME || isNewerPackageVersion(latestRelease.version, VERSION)) {
		return {
			packageName,
			installSpec,
			version: latestRelease.version,
			...(latestRelease.note ? { note: latestRelease.note } : {}),
			shouldRun: true,
		};
	}

	console.log(chalk.green(`${APP_NAME} is already up to date (v${VERSION})`));
	return { packageName, installSpec, version: latestRelease.version, shouldRun: false };
}

async function runSelfUpdate(command: SelfUpdateCommand): Promise<void> {
	console.log(chalk.dim(`Updating ${APP_NAME} with ${command.display}...`));
	for (const step of command.steps ?? [command]) {
		await new Promise<void>((resolve, reject) => {
			const child = spawnProcess(step.command, step.args, { stdio: "inherit" });
			child.on("error", reject);
			child.on("close", (code, signal) => {
				if (code === 0) resolve();
				else if (signal) reject(new Error(`${step.display} terminated by signal ${signal}`));
				else reject(new Error(`${step.display} exited with code ${code ?? "unknown"}`));
			});
		});
	}
}

function prepareWindowsNpmSelfUpdate(): void {
	if (process.platform !== "win32") return;
	const packageDir = getPackageDir();
	cleanupWindowsSelfUpdateQuarantine(packageDir);
	quarantineWindowsNativeDependencies(packageDir);
}

/** Bone exposes only its own update command; third-party package commands are gone. */
export async function handlePackageCommand(args: string[]): Promise<boolean> {
	const [command, ...rest] = args;
	if (
		command === "install" ||
		command === "remove" ||
		command === "uninstall" ||
		command === "list" ||
		command === "config"
	) {
		console.error(
			chalk.red(`${APP_NAME} ${command} is not available; Bone does not load third-party packages or extensions.`),
		);
		process.exitCode = 1;
		return true;
	}
	if (command !== "update") return false;

	const force = rest.includes("--force");
	const unsupported = rest.find((arg) => ["--extensions", "--extension", "--all", "--models"].includes(arg));
	if (unsupported) {
		console.error(chalk.red(`${unsupported} is not available; \`${APP_NAME} update\` updates Bone only.`));
		process.exitCode = 1;
		return true;
	}
	if (rest.some((arg) => arg === "--self" || arg === "self" || arg === "bone")) {
		// Explicit self aliases are accepted for scripts that already use them.
	}

	try {
		const selfUpdatePlan = await getSelfUpdatePlan(force);
		if (!selfUpdatePlan.shouldRun) return true;
		const installMethod = detectInstallMethod();
		if (process.platform === "win32" && installMethod !== "npm" && installMethod !== "pnpm") {
			console.error(chalk.red(`${APP_NAME} self-update on Windows is only supported for npm and pnpm installs.`));
			process.exitCode = 1;
			return true;
		}
		const selfUpdateCommand = getSelfUpdateCommand(PACKAGE_NAME, undefined, {
			packageName: selfUpdatePlan.packageName,
			installSpec: selfUpdatePlan.installSpec,
		});
		if (!selfUpdateCommand) {
			printSelfUpdateUnavailable(undefined, {
				packageName: selfUpdatePlan.packageName,
				installSpec: selfUpdatePlan.installSpec,
			});
			process.exitCode = 1;
			return true;
		}
		if (selfUpdatePlan.note) printSelfUpdateNote(selfUpdatePlan.note);
		try {
			if (installMethod === "npm") prepareWindowsNpmSelfUpdate();
			await runSelfUpdate(selfUpdateCommand);
		} catch (error: unknown) {
			const message = error instanceof Error ? error.message : "Unknown update error";
			console.error(chalk.red(`Error: ${message}`));
			if (installMethod === "pnpm") printPnpmSelfUpdateMetadataHint();
			printSelfUpdateFallback(selfUpdateCommand);
			process.exitCode = 1;
			return true;
		}
		console.log(chalk.green(`Updated ${APP_NAME} from ${VERSION} to ${selfUpdatePlan.version}`));
	} catch (error: unknown) {
		const message = error instanceof Error ? error.message : "Unknown update error";
		console.error(chalk.red(`Error: ${message}`));
		process.exitCode = 1;
	}
	return true;
}
