/**
 * Run modes for the coding agent.
 */

import type { ImageContent } from "@frelion/bone-ai/compat";
import { OpenTUIInteractiveMode, type OpenTUISessionHostContract } from "./interactive/opentui-interactive-mode.ts";

export interface InteractiveModeOptions {
	migratedProviders?: string[];
	modelFallbackMessage?: string;
	autoTrustOnReloadCwd?: string;
	initialMessage?: string;
	initialImages?: ImageContent[];
	initialMessages?: string[];
	verbose?: boolean;
}

/** Public interactive entry point. OpenTUI implementation hooks remain internal. */
export class InteractiveMode extends OpenTUIInteractiveMode {
	constructor(sessionHost: OpenTUISessionHostContract, options: InteractiveModeOptions = {}) {
		super(sessionHost, options);
	}
}
export { type PrintModeOptions, runPrintMode } from "./print-mode.ts";
export { type ModelInfo, RpcClient, type RpcClientOptions, type RpcEventListener } from "./rpc/rpc-client.ts";
export { runRpcMode } from "./rpc/rpc-mode.ts";
export type {
	RpcCommand,
	RpcExtensionUIRequest,
	RpcExtensionUIResponse,
	RpcResponse,
	RpcSessionState,
} from "./rpc/rpc-types.ts";
