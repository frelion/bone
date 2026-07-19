import { access, cp, mkdir } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const rootDirectory = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const destinationRoot = resolve(process.cwd(), process.argv[2] ?? "dist");

function targetTriplets() {
	if (process.platform === "darwin" && process.arch === "arm64") return ["darwin_arm64"];
	if (process.platform === "darwin" && process.arch === "x64") return ["darwin_x64"];
	if (process.platform === "linux" && process.arch === "arm64") return ["linux_arm64", "musl_arm64"];
	if (process.platform === "linux" && process.arch === "x64") return ["linux_x64", "musl_x64"];
	if (process.platform === "win32" && process.arch === "arm64") return ["win32_arm64"];
	if (process.platform === "win32" && process.arch === "x64") return ["win32_x64"];
	throw new Error(`Koffi binary packaging is unsupported for ${process.platform}-${process.arch}`);
}

for (const triplet of targetTriplets()) {
	const source = join(rootDirectory, "node_modules", "koffi", "build", "koffi", triplet, "koffi.node");
	try {
		await access(source);
	} catch {
		throw new Error(`Koffi native binding is missing: ${source}`);
	}
	const destination = join(destinationRoot, "node_modules", "koffi", "build", "koffi", triplet, "koffi.node");
	await mkdir(dirname(destination), { recursive: true });
	await cp(source, destination);
}
