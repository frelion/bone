import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

const aiSrcIndex = fileURLToPath(new URL("../ai/src/index.ts", import.meta.url));
const aiSrcCompat = fileURLToPath(new URL("../ai/src/compat.ts", import.meta.url));
const aiSrcOAuth = fileURLToPath(new URL("../ai/src/oauth.ts", import.meta.url));
const aiSrcProviders = fileURLToPath(new URL("../ai/src/providers", import.meta.url));
const agentSrcIndex = fileURLToPath(new URL("../agent/src/index.ts", import.meta.url));
const tuiSrcIndex = fileURLToPath(new URL("../tui/src/index.ts", import.meta.url));

export default defineConfig({
	test: {
		globals: true,
		environment: "node",
		// OpenTUI 0.4.5 owns a process-global TreeSitter client. Parallel test
		// files can otherwise destroy the client while another renderer uses it.
		fileParallelism: false,
		testTimeout: 30000,
		reporters: process.env.GITHUB_ACTIONS ? ["dot", "github-actions"] : ["dot"],
		silent: "passed-only",
		server: {
			deps: {
				external: [/@silvia-odwyer\/photon-node/],
			},
		},
	},
	resolve: {
		alias: [
			{ find: /^@frelion\/bone-ai$/, replacement: aiSrcIndex },
			{ find: /^@frelion\/bone-ai\/compat$/, replacement: aiSrcCompat },
			{ find: /^@frelion\/bone-ai\/oauth$/, replacement: aiSrcOAuth },
			{ find: /^@frelion\/bone-ai\/providers\/(.+)$/, replacement: `${aiSrcProviders}/$1.ts` },
			{ find: /^@frelion\/bone-agent-core$/, replacement: agentSrcIndex },
			{ find: /^@frelion\/bone-tui$/, replacement: tuiSrcIndex },
		],
	},
});
