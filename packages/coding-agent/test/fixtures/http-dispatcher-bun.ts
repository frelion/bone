import { createServer } from "node:http";
import { request } from "undici-client";
import { createPrivateNetworkDispatcher } from "../../src/core/forge/network-security.ts";
import { applyHttpProxySettings } from "../../src/core/http-dispatcher.ts";

const mode = process.argv[2];
const originalFetch = globalThis.fetch;

if (mode === "sse") {
	let sendTerminalEvent: (() => void) | undefined;
	const terminalEventGate = new Promise<void>((resolve) => {
		sendTerminalEvent = resolve;
	});
	const server = createServer(async (_request, response) => {
		response.writeHead(200, {
			"content-type": "text/event-stream",
			connection: "keep-alive",
		});
		response.flushHeaders();
		response.write('event: response.created\ndata: {"type":"response.created"}\n\n');
		await terminalEventGate;
		response.end('event: response.completed\ndata: {"type":"response.completed"}\n\n');
	});
	await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));

	try {
		const address = server.address();
		if (!address || typeof address === "string") throw new Error("Expected TCP server address");
		const response = await fetch(`http://127.0.0.1:${address.port}`);
		if (!response.body) throw new Error("Expected SSE response body");
		const reader = response.body.getReader();
		const decoder = new TextDecoder();
		const first = await reader.read();
		const firstChunk = first.value ? decoder.decode(first.value, { stream: true }) : "";
		let body = firstChunk;
		sendTerminalEvent?.();
		while (true) {
			const chunk = await reader.read();
			if (chunk.done) break;
			if (chunk.value) body += decoder.decode(chunk.value, { stream: true });
		}
		body += decoder.decode();
		console.log(JSON.stringify({ fetchPreserved: globalThis.fetch === originalFetch, firstChunk, body }));
	} finally {
		sendTerminalEvent?.();
		await new Promise<void>((resolve, reject) => {
			server.close((error) => (error ? reject(error) : resolve()));
		});
	}
} else if (mode === "proxy") {
	const proxiedUrls: string[] = [];
	const tunneledHosts: string[] = [];
	const proxy = createServer((request, response) => {
		if (request.url) proxiedUrls.push(request.url);
		response.end("proxied");
	});
	proxy.on("connect", (request, socket) => {
		if (request.url) tunneledHosts.push(request.url);
		socket.write("HTTP/1.1 200 Connection Established\r\n\r\n");
		socket.once("data", () => {
			socket.end("HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nproxied");
		});
	});
	await new Promise<void>((resolve) => proxy.listen(0, "127.0.0.1", resolve));

	try {
		const address = proxy.address();
		if (!address || typeof address === "string") throw new Error("Expected TCP proxy address");
		applyHttpProxySettings(`http://127.0.0.1:${address.port}`);
		const response = await fetch("http://bone-proxy-check.invalid/path?q=1");
		const forgeDispatcher = createPrivateNetworkDispatcher();
		const forgeResponse = await request("http://bone-forge-proxy-check.invalid/api/v4/user", {
			dispatcher: forgeDispatcher,
		});
		const forgeBody = await forgeResponse.body.text();
		await forgeDispatcher.close();
		console.log(
			JSON.stringify({
				fetchPreserved: globalThis.fetch === originalFetch,
				body: await response.text(),
				forgeBody,
				proxiedUrls,
				tunneledHosts,
			}),
		);
	} finally {
		const closed = new Promise<void>((resolve, reject) => {
			proxy.close((error) => (error ? reject(error) : resolve()));
		});
		proxy.closeAllConnections();
		await closed;
	}
} else {
	throw new Error(`Unknown fixture mode: ${String(mode)}`);
}
