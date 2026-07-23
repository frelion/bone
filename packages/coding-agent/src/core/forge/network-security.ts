import { lookup as dnsLookup } from "node:dns";
import { isIP } from "node:net";
import { Agent, type Dispatcher } from "undici-client";
import { ForgeError } from "./errors.ts";

function ipv4Number(address: string): number | undefined {
	const parts = address.split(".").map(Number);
	if (parts.length !== 4 || parts.some((part) => !Number.isInteger(part) || part < 0 || part > 255)) return undefined;
	return (((parts[0] << 24) >>> 0) + (parts[1] << 16) + (parts[2] << 8) + parts[3]) >>> 0;
}

function inIpv4Range(address: number, base: number, prefix: number): boolean {
	const mask = prefix === 0 ? 0 : (0xffffffff << (32 - prefix)) >>> 0;
	return (address & mask) === (base & mask);
}

export function isPrivateNetworkAddress(address: string): boolean {
	const normalized = address.toLowerCase().split("%")[0];
	if (isIP(normalized) === 4) {
		const value = ipv4Number(normalized);
		if (value === undefined) return true;
		return [
			["0.0.0.0", 8],
			["10.0.0.0", 8],
			["100.64.0.0", 10],
			["127.0.0.0", 8],
			["169.254.0.0", 16],
			["172.16.0.0", 12],
			["192.0.0.0", 24],
			["192.168.0.0", 16],
			["198.18.0.0", 15],
			["224.0.0.0", 4],
			["240.0.0.0", 4],
		].some(([base, prefix]) => inIpv4Range(value, ipv4Number(String(base)) ?? 0, Number(prefix)));
	}
	if (isIP(normalized) === 6) {
		return !/^[23]/.test(normalized) || normalized.startsWith("::ffff:") || normalized.startsWith("2001:db8:");
	}
	return true;
}

export function assertPublicNetworkHostname(hostname: string): void {
	const normalized = hostname.replace(/^\[|\]$/g, "");
	if (isIP(normalized) !== 0 && isPrivateNetworkAddress(normalized)) {
		throw new ForgeError("unsafe_remote", `Forge host ${hostname} is on a private network`);
	}
}

export function createPublicNetworkDispatcher(): Dispatcher {
	return new Agent({
		connect: {
			lookup(hostname, options, callback) {
				const normalized = hostname.replace(/^\[|\]$/g, "");
				dnsLookup(normalized, { ...options, all: true }, (error, addresses) => {
					if (error) {
						callback(error, "", 4);
						return;
					}
					if (addresses.length === 0 || addresses.some(({ address }) => isPrivateNetworkAddress(address))) {
						callback(
							new ForgeError("unsafe_remote", `Forge host ${hostname} resolved to a private network`),
							"",
							4,
						);
						return;
					}
					const preferred = addresses.find(({ family }) => family === options.family) ?? addresses[0];
					callback(null, preferred.address, preferred.family);
				});
			},
		},
	});
}
