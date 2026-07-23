import { basename } from "node:path";
import chalk from "chalk";
import { MultiBar, type SingleBar } from "cli-progress";
import { getLocalEmbeddingAvailability, type LocalEmbeddingStatus } from "../core/local-embedding.ts";

const MAX_PROGRESS_FILE_NAME_WIDTH = 18;

interface BunWorker {
	onmessage: ((event: { data: unknown }) => void) | null;
	onerror: ((event: { message: string }) => void) | null;
	postMessage(message: unknown): void;
	terminate(): void;
}

declare const Worker: {
	new (specifier: string | URL, options: { type: "module" }): BunWorker;
};

function formatBytes(bytes: number): string {
	if (bytes < 1024 * 1024) return `${Math.max(0, Math.round(bytes / 1024))} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(bytes >= 100 * 1024 * 1024 ? 0 : 1)} MB`;
}

function formatFileName(file: string): string {
	const name = basename(file);
	if (name.length <= MAX_PROGRESS_FILE_NAME_WIDTH) return name;
	return `${name.slice(0, MAX_PROGRESS_FILE_NAME_WIDTH - 1)}…`;
}

interface DownloadProgressBar {
	bar: SingleBar;
	totalBytes: number;
}

function setupWorkerEntry(): string | URL {
	// Bun standalone executables resolve explicitly bundled worker entrypoints
	// by their source path. Resolving a URL from the compiled CLI instead
	// targets a different virtual filesystem path.
	if (typeof process.versions.bun === "string" && !import.meta.url.startsWith("file:")) {
		return "./src/core/local-embedding-setup-worker.ts";
	}
	return new URL("../core/local-embedding-setup-worker.js", import.meta.url);
}

function asStatus(value: unknown): LocalEmbeddingStatus | undefined {
	if (!value || typeof value !== "object") return undefined;
	const candidate = value as { phase?: unknown; loadedBytes?: unknown; totalBytes?: unknown; file?: unknown };
	if (candidate.phase === "loading" || candidate.phase === "ready") return { phase: candidate.phase };
	if (candidate.phase !== "downloading") return undefined;
	return {
		phase: "downloading",
		...(typeof candidate.loadedBytes === "number" && Number.isFinite(candidate.loadedBytes)
			? { loadedBytes: candidate.loadedBytes }
			: {}),
		...(typeof candidate.totalBytes === "number" && Number.isFinite(candidate.totalBytes)
			? { totalBytes: candidate.totalBytes }
			: {}),
		...(typeof candidate.file === "string" ? { file: candidate.file } : {}),
	};
}

/** Download and verify the fixed GGUF asset without loading it into normal Bone startup. */
async function prepareLocalEmbeddingAssets(
	agentDir: string,
	onStatus: (status: LocalEmbeddingStatus) => void,
): Promise<void> {
	const worker = new Worker(setupWorkerEntry(), { type: "module" });
	try {
		await new Promise<void>((resolve, reject) => {
			let settled = false;
			const finish = (callback: () => void) => {
				if (settled) return;
				settled = true;
				callback();
			};
			worker.onmessage = (event) => {
				const message = event.data;
				if (!message || typeof message !== "object") return;
				const candidate = message as { type?: unknown; status?: unknown; message?: unknown };
				if (candidate.type === "status") {
					const status = asStatus(candidate.status);
					if (status) onStatus(status);
					return;
				}
				if (candidate.type === "complete") finish(resolve);
				if (candidate.type === "error") {
					finish(() =>
						reject(new Error(typeof candidate.message === "string" ? candidate.message : "Model setup failed")),
					);
				}
			};
			worker.onerror = (event) => finish(() => reject(new Error(event.message)));
			worker.postMessage({ agentDir });
		});
	} finally {
		worker.terminate();
	}
}

/** Renders each concurrently fetched model asset as an independent progress bar. */
class SetupProgressReporter {
	private readonly interactive = Boolean(process.stdout.isTTY);
	private lastNonInteractiveStage = "";
	private bars: MultiBar | undefined;
	private readonly files = new Map<string, DownloadProgressBar>();
	private loadingShown = false;

	report(status: LocalEmbeddingStatus): void {
		// handleSetupCommand prints the final, verified completion line itself.
		if (status.phase === "ready") return;
		if (!this.interactive) {
			// In a pipe or CI log, keep phase transitions but never print every percentage update.
			const stage = status.phase === "downloading" ? "download" : status.phase;
			if (stage === this.lastNonInteractiveStage) return;
			this.lastNonInteractiveStage = stage;
			console.log(
				status.phase === "loading" ? "Loading local semantic model…" : "Downloading local semantic model…",
			);
			return;
		}

		if (status.phase === "loading") {
			this.stopBars();
			if (!this.loadingShown) {
				this.loadingShown = true;
				console.log("Loading local semantic model…");
			}
			return;
		}
		this.updateFile(status);
	}

	finish(): void {
		this.stopBars();
	}

	private updateFile(status: Extract<LocalEmbeddingStatus, { phase: "downloading" }>): void {
		if (!status.file || !status.totalBytes || status.loadedBytes === undefined) return;
		const totalBytes = Math.max(1, status.totalBytes);
		const loadedBytes = Math.min(Math.max(0, status.loadedBytes), totalBytes);
		let progress = this.files.get(status.file);
		if (!progress) {
			const bar = this.getBars().create(totalBytes, loadedBytes, {
				file: formatFileName(status.file),
				loaded: formatBytes(loadedBytes),
				size: formatBytes(totalBytes),
			});
			progress = { bar, totalBytes };
			this.files.set(status.file, progress);
			return;
		}
		if (progress.totalBytes !== totalBytes) {
			progress.bar.setTotal(totalBytes);
			progress.totalBytes = totalBytes;
		}
		progress.bar.update(loadedBytes, {
			file: formatFileName(status.file),
			loaded: formatBytes(loadedBytes),
			size: formatBytes(totalBytes),
		});
	}

	private getBars(): MultiBar {
		if (!this.bars) {
			this.bars = new MultiBar({
				format: " {file}  [{bar}] {percentage}%  {loaded} / {size}",
				barCompleteChar: "=",
				barIncompleteChar: "-",
				barsize: 22,
				hideCursor: true,
				clearOnComplete: false,
				stopOnComplete: false,
				fps: 12,
				stream: process.stdout,
			});
		}
		return this.bars;
	}

	private stopBars(): void {
		if (!this.bars) return;
		this.bars.stop();
		this.bars = undefined;
	}
}

/** Handle `bone setup`: the sole explicit download path for local semantic search assets. */
export async function handleSetupCommand(args: string[], agentDir: string): Promise<boolean> {
	if (args[0] !== "setup") return false;
	if (args[1] === "--help" || args[1] === "-h") {
		console.log("Usage: bone setup\n\nDownloads and verifies Bone's local semantic-search model.");
		return true;
	}
	if (args.length > 1) {
		console.error(chalk.red("Error: bone setup does not accept arguments."));
		process.exitCode = 1;
		return true;
	}

	const existing = await getLocalEmbeddingAvailability(agentDir);
	if (existing.state === "ready") {
		console.log("Local semantic model is already installed and verified.");
		return true;
	}

	console.log(existing.state === "missing" ? "Installing local semantic model…" : "Repairing local semantic model…");
	const progress = new SetupProgressReporter();
	try {
		await prepareLocalEmbeddingAssets(agentDir, (status) => progress.report(status));
		const verified = await getLocalEmbeddingAvailability(agentDir);
		if (verified.state !== "ready") throw new Error("Downloaded semantic model failed verification");
		progress.finish();
		console.log(chalk.green("Semantic search is ready."));
	} catch (error) {
		progress.finish();
		const message = error instanceof Error ? error.message : String(error);
		console.error(chalk.red(`Failed to prepare local semantic search: ${message}`));
		process.exitCode = 1;
	}
	return true;
}
