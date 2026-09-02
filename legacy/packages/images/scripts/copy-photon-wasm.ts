#!/usr/bin/env bun

import { fileURLToPath } from "node:url";

const destination = process.argv[2];
if (!destination) {
	throw new Error("Usage: bun scripts/copy-photon-wasm.ts <destination>");
}

const source = fileURLToPath(import.meta.resolve("@silvia-odwyer/photon-node/photon_rs_bg.wasm"));
await Bun.write(destination, Bun.file(source));
