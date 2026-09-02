import { resolveRenderLib } from "@opentui/core";

/** Resolve the bundled OpenTUI native library and execute a native call. */
export function verifyOpenTUINativeRuntime(): void {
	resolveRenderLib().getBuildOptions();
}
