const bunVersion = process.versions.bun;
const [major = 0, minor = 0, patch = 0] = bunVersion?.split(".").map(Number) ?? [];
if (!bunVersion || major < 1 || (major === 1 && (minor < 3 || (minor === 3 && patch < 14)))) {
	const detectedRuntime = bunVersion ? `Bun ${bunVersion}` : "Node.js";
	throw new Error(`Bone requires Bun 1.3.14 or newer. Detected ${detectedRuntime}, which is not supported.`);
}
