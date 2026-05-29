import { RpcError, type RpcErrorPayload } from "./error";

export { RpcError };
export type { RpcErrorPayload };

const RPC_BASE = "/api/rpc";
const WS_PATH = "/api/ws";

export async function call<T>(
	command: string,
	args?: Record<string, unknown>,
): Promise<T> {
	const res = await fetch(`${RPC_BASE}/${command}`, {
		method: "POST",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify(args ?? {}),
	});
	if (!res.ok) {
		const body = await res.json().catch(() => ({
			kind: "Network",
			message: `${res.status} ${res.statusText}`,
		}));
		if (isErrorPayload(body)) {
			throw new RpcError(body);
		}
		throw new RpcError({ kind: "Unknown", message: JSON.stringify(body) });
	}
	if (res.status === 204) {
		return undefined as T;
	}
	return (await res.json()) as T;
}

export type UnlistenFn = () => void;

interface EventFrame {
	type: "event";
	topic: string;
	payload: unknown;
}

type Handler = (payload: unknown) => void;

let socket: WebSocket | null = null;
let socketReady: Promise<WebSocket> | null = null;
let reconnectAttempt = 0;
const handlers = new Map<string, Set<Handler>>();
const topicCounts = new Map<string, number>();

function wsUrl(): string {
	const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
	return `${proto}//${window.location.host}${WS_PATH}`;
}

function connect(): Promise<WebSocket> {
	if (socketReady) return socketReady;
	socketReady = new Promise((resolve, reject) => {
		const ws = new WebSocket(wsUrl());
		ws.onopen = () => {
			socket = ws;
			reconnectAttempt = 0;
			for (const topic of topicCounts.keys()) {
				ws.send(JSON.stringify({ type: "subscribe", topic }));
			}
			resolve(ws);
		};
		ws.onmessage = (e) => {
			const frame = parseFrame(e.data);
			if (!frame) return;
			const set = handlers.get(frame.topic);
			if (!set) return;
			for (const h of set) {
				try {
					h(frame.payload);
				} catch (err) {
					console.error("rpc handler threw", err);
				}
			}
		};
		ws.onerror = () => {};
		ws.onclose = () => {
			socket = null;
			socketReady = null;
			if (handlers.size > 0) {
				const delay = Math.min(30_000, 500 * 2 ** reconnectAttempt);
				reconnectAttempt += 1;
				setTimeout(() => {
					connect().catch(() => {});
				}, delay);
			}
			reject(new Error("websocket closed"));
		};
	});
	return socketReady;
}

function parseFrame(data: unknown): EventFrame | null {
	if (typeof data !== "string") return null;
	try {
		const f = JSON.parse(data);
		if (f && f.type === "event" && typeof f.topic === "string") {
			return f as EventFrame;
		}
	} catch {
		// ignored
	}
	return null;
}

export function listen<T>(
	topic: string,
	handler: (payload: T) => void,
): UnlistenFn {
	const wrapped: Handler = (p) => handler(p as T);
	let set = handlers.get(topic);
	if (!set) {
		set = new Set();
		handlers.set(topic, set);
	}
	set.add(wrapped);

	const prevCount = topicCounts.get(topic) ?? 0;
	topicCounts.set(topic, prevCount + 1);
	if (prevCount === 0) {
		connect()
			.then((ws) => {
				ws.send(JSON.stringify({ type: "subscribe", topic }));
			})
			.catch(() => {});
	}

	return () => {
		const cur = handlers.get(topic);
		if (!cur) return;
		cur.delete(wrapped);
		const count = (topicCounts.get(topic) ?? 1) - 1;
		if (count <= 0) {
			topicCounts.delete(topic);
			handlers.delete(topic);
			if (socket && socket.readyState === WebSocket.OPEN) {
				socket.send(JSON.stringify({ type: "unsubscribe", topic }));
			}
		} else {
			topicCounts.set(topic, count);
		}
	};
}

function isErrorPayload(value: unknown): value is RpcErrorPayload {
	return (
		typeof value === "object" &&
		value !== null &&
		"kind" in value &&
		typeof (value as { kind: unknown }).kind === "string"
	);
}
