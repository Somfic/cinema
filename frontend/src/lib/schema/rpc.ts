import {
	type Conn,
	defaultRpc,
	RpcError as DraadRpcError,
	type HttpVerb,
} from "./index";
import { RpcError, type RpcErrorPayload } from "./error";

export { RpcError };
export type { RpcErrorPayload };
export type { UnlistenFn } from "./index";

const WS_URL =
	typeof window !== "undefined"
		? `${window.location.protocol === "https:" ? "wss:" : "ws:"}//${window.location.host}/api/ws`
		: undefined;

const rpc: Conn = defaultRpc({ baseUrl: "/api/rpc", wsUrl: WS_URL });


export const conn = rpc;

export async function call<T>(
	command: string,
	args?: Record<string, unknown>,
	method?: HttpVerb,
): Promise<T> {
	try {
		return await rpc.call<T>(command, args, method);
	} catch (e) {
		if (e instanceof DraadRpcError) throw toRpcError(e);
		throw e;
	}
}

export function listen<T>(
	topic: string,
	handler: (payload: T) => void,
): () => void {
	return rpc.listen<T>(topic, handler);
}

export function onOpen(handler: () => void): () => void {
	return rpc.onOpen!(handler);
}

export function publish(topic: string, payload: unknown): void {
	rpc.send!({ type: "publish", topic, payload });
}

export function announce(
	presence: unknown,
	kind: "register" | "update" = "update",
): void {
	rpc.send!({ type: kind, presence });
}

function toRpcError(e: DraadRpcError): RpcError {
	const body = e.body;
	if (body && typeof body === "object") {
		const o = body as Record<string, unknown>;
		return new RpcError({
			kind: typeof o.kind === "string" ? o.kind : e.code,
			message: typeof o.message === "string" ? o.message : e.message,
		});
	}
	return new RpcError({ kind: e.code, message: e.message });
}
