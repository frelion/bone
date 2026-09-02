import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

const aiSrcIndex = fileURLToPath(new URL("../ai/src/index.ts", import.meta.url));
const agentSrcIndex = fileURLToPath(new URL("../agent/src/index.ts", import.meta.url));
const sessionSrcIndex = fileURLToPath(new URL("../session/src/index.ts", import.meta.url));

export default defineConfig({
	test: {
		globals: true,
		environment: "node",
		testTimeout: 30000,
		reporters: process.env.GITHUB_ACTIONS ? ["dot", "github-actions"] : ["dot"],
		silent: "passed-only",
	},
	resolve: {
		alias: [
			{ find: /^@frelion\/bone-ai$/, replacement: aiSrcIndex },
			{ find: /^@frelion\/bone-agent-core$/, replacement: agentSrcIndex },
			{ find: /^@frelion\/bone-session$/, replacement: sessionSrcIndex },
		],
	},
});
