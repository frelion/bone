declare module "bun:ffi" {
	export type Pointer = number & { readonly __pointer__: unique symbol };

	export class CString {
		constructor(pointer: Pointer);
		toString(): string;
	}

	export class JSCallback {
		readonly ptr: Pointer;
		constructor(callback: (...args: (Pointer | null)[]) => void, definition: unknown);
		close(): void;
	}

	export function dlopen(path: string | URL, symbols: Readonly<Record<string, unknown>>): unknown;
	export function ptr(value: ArrayBuffer | ArrayBufferView): Pointer;
	export function toArrayBuffer(pointer: Pointer, byteOffset: number, byteLength: number): ArrayBuffer;
}
