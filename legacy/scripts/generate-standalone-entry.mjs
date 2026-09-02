#!/usr/bin/env bun
import { createHash } from "node:crypto";
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { createRequire } from "node:module";

const PLATFORM_CONFIG = {
	"darwin-arm64": {
		nativeTarget: "darwin-arm64",
		clipboardPackage: "clipboard-darwin-arm64",
		clipboardFile: "clipboard.darwin-arm64.node",
	},
	"darwin-x64": {
		nativeTarget: "darwin-x64",
		clipboardPackage: "clipboard-darwin-x64",
		clipboardFile: "clipboard.darwin-x64.node",
	},
	"linux-arm64": {
		nativeTarget: "linux-arm64",
		clipboardPackage: "clipboard-linux-arm64-gnu",
		clipboardFile: "clipboard.linux-arm64-gnu.node",
	},
	"linux-x64": {
		nativeTarget: "linux-x64",
		clipboardPackage: "clipboard-linux-x64-gnu",
		clipboardFile: "clipboard.linux-x64-gnu.node",
	},
	"windows-arm64": {
		nativeTarget: "win32-arm64",
		clipboardPackage: "clipboard-win32-arm64-msvc",
		clipboardFile: "clipboard.win32-arm64-msvc.node",
	},
	"windows-x64": {
		nativeTarget: "win32-x64",
		clipboardPackage: "clipboard-win32-x64-msvc",
		clipboardFile: "clipboard.win32-x64-msvc.node",
	},
};

function parseArgs(argv) {
	const options = {};
	for (let index = 0; index < argv.length; index += 2) {
		const flag = argv[index];
		const value = argv[index + 1];
		if (!flag?.startsWith("--") || !value) throw new Error(`Invalid argument near ${flag ?? "<end>"}`);
		options[flag.slice(2)] = value;
	}
	if (!options.platform || !options.out) throw new Error("Usage: generate-standalone-entry.mjs --platform <target> --out <file>");
	return options;
}

function walkFiles(path) {
	const stat = lstatSync(path);
	if (stat.isSymbolicLink()) return walkFiles(realpathSync(path));
	if (stat.isFile()) return [realpathSync(path)];
	if (!stat.isDirectory()) return [];
	return readdirSync(path, { withFileTypes: true })
		.sort((left, right) => left.name.localeCompare(right.name))
		.flatMap((entry) => walkFiles(join(path, entry.name)));
}

const options = parseArgs(process.argv.slice(2));
const root = resolve(import.meta.dir, "..");
const outputPath = resolve(options.out);
const outputDirectory = dirname(outputPath);
const config = PLATFORM_CONFIG[options.platform];
if (!config) throw new Error(`Unsupported standalone target: ${options.platform}`);

const codingAgentDirectory = join(root, "packages", "coding-agent");
const imagesDirectory = join(root, "packages", "images");
const requireFromImages = createRequire(join(imagesDirectory, "package.json"));
const clipboardBaseDirectory = dirname(requireFromImages.resolve("@mariozechner/clipboard"));
const requireFromClipboard = createRequire(join(clipboardBaseDirectory, "package.json"));
const clipboardNativeName = `@mariozechner/${config.clipboardPackage}`;
const clipboardNativeDirectory = dirname(requireFromClipboard.resolve(clipboardNativeName));
const photonWasmPath = requireFromImages.resolve("@silvia-odwyer/photon-node/photon_rs_bg.wasm");

const assets = [];
function addFile(source, destination) {
	const normalizedDestination = destination.split(sep).join("/");
	if (assets.some((asset) => asset.destination === normalizedDestination)) return;
	if (!existsSync(source)) throw new Error(`Standalone asset is missing: ${source}`);
	assets.push({ source: realpathSync(source), destination: normalizedDestination });
}

function addDirectory(source, destination) {
	for (const file of walkFiles(source)) {
		addFile(file, join(destination, relative(realpathSync(source), file)));
	}
}

addFile(join(codingAgentDirectory, "package.json"), "package.json");
addFile(join(codingAgentDirectory, "README.md"), "README.md");
addFile(join(codingAgentDirectory, "CHANGELOG.md"), "CHANGELOG.md");
addFile(photonWasmPath, "photon_rs_bg.wasm");
for (const themeName of ["dark.json", "light.json", "theme-schema.json"]) {
	addFile(join(codingAgentDirectory, "dist", "modes", "interactive", "theme", themeName), join("theme", themeName));
}
addDirectory(join(codingAgentDirectory, "dist", "modes", "interactive", "assets"), "assets");
for (const templateName of ["template.html", "template.css", "template.js"]) {
	addFile(join(codingAgentDirectory, "dist", "core", "export-html", templateName), join("export-html", templateName));
}
addDirectory(join(codingAgentDirectory, "dist", "core", "export-html", "vendor"), join("export-html", "vendor"));
addDirectory(join(codingAgentDirectory, "docs"), "docs");
addDirectory(join(codingAgentDirectory, "examples"), "examples");
addDirectory(join(root, "packages", "memory", "native", config.nativeTarget), join("native", config.nativeTarget));
for (const clipboardFile of ["index.js", "index.d.ts", "package.json"]) {
	addFile(
		join(clipboardBaseDirectory, clipboardFile),
		join("node_modules", "@mariozechner", "clipboard", clipboardFile),
	);
}
addFile(
	join(clipboardNativeDirectory, config.clipboardFile),
	join("node_modules", "@mariozechner", "clipboard", config.clipboardFile),
);

assets.sort((left, right) => left.destination.localeCompare(right.destination));
const digest = createHash("sha256");
for (const asset of assets) {
	digest.update(asset.destination);
	digest.update("\0");
	digest.update(readFileSync(asset.source));
	digest.update("\0");
}
const manifestId = digest.digest("hex").slice(0, 20);
const packageJson = JSON.parse(readFileSync(join(codingAgentDirectory, "package.json"), "utf8"));

mkdirSync(outputDirectory, { recursive: true });
const canonicalOutputDirectory = realpathSync(outputDirectory);
const imports = assets
	.map((asset, index) => {
		const specifier = relative(canonicalOutputDirectory, asset.source).split(sep).join("/");
		return `import asset${index} from ${JSON.stringify(specifier.startsWith(".") ? specifier : `./${specifier}`)} with { type: "file" };`;
	})
	.join("\n");
const entries = assets.map((asset, index) => `\t[${JSON.stringify(asset.destination)}, asset${index}],`).join("\n");
const cliSpecifier = relative(canonicalOutputDirectory, join(codingAgentDirectory, "dist", "bun", "cli.js"))
	.split(sep)
	.join("/");

const source = `${imports}
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

const manifestId = ${JSON.stringify(manifestId)};
const version = ${JSON.stringify(packageJson.version)};
const embeddedAssets = [
${entries}
] as const;

function cacheRoot(): string {
	if (process.env.BONE_RUNTIME_CACHE_DIR) return process.env.BONE_RUNTIME_CACHE_DIR;
	if (process.platform === "win32" && process.env.LOCALAPPDATA) return join(process.env.LOCALAPPDATA, "Bone", "Cache");
	if (process.platform === "darwin") return join(homedir(), "Library", "Caches", "Bone");
	return join(process.env.XDG_CACHE_HOME || join(homedir(), ".cache"), "bone");
}

async function materializeRuntime(): Promise<string> {
	const runtimeDirectory = join(
		cacheRoot(),
		"runtime",
		\`\${version}-\${process.platform}-\${process.arch}-\${manifestId}\`,
	);
	const marker = join(runtimeDirectory, ".complete");
	if (existsSync(marker) && readFileSync(marker, "utf8") === manifestId) return runtimeDirectory;

	const temporaryDirectory = \`\${runtimeDirectory}.tmp-\${process.pid}-\${Math.random().toString(16).slice(2)}\`;
	rmSync(temporaryDirectory, { recursive: true, force: true });
	mkdirSync(temporaryDirectory, { recursive: true });
	try {
		for (const [destination, embeddedPath] of embeddedAssets) {
			const outputPath = join(temporaryDirectory, destination);
			mkdirSync(dirname(outputPath), { recursive: true });
			await Bun.write(outputPath, Bun.file(embeddedPath));
		}
		await Bun.write(join(temporaryDirectory, ".complete"), manifestId);
		try {
			renameSync(temporaryDirectory, runtimeDirectory);
		} catch {
			if (existsSync(marker) && readFileSync(marker, "utf8") === manifestId) {
				rmSync(temporaryDirectory, { recursive: true, force: true });
			} else {
				rmSync(runtimeDirectory, { recursive: true, force: true });
				renameSync(temporaryDirectory, runtimeDirectory);
			}
		}
	} catch (error) {
		rmSync(temporaryDirectory, { recursive: true, force: true });
		throw error;
	}
	return runtimeDirectory;
}

const runtimeDirectory = await materializeRuntime();
process.env.BONE_RUNTIME_DIR = runtimeDirectory;
process.env.BONE_PACKAGE_DIR ??= runtimeDirectory;
await import(${JSON.stringify(cliSpecifier.startsWith(".") ? cliSpecifier : `./${cliSpecifier}`)});
`;

writeFileSync(outputPath, source);
console.log(`Generated ${outputPath} with ${assets.length} embedded assets (${manifestId})`);
