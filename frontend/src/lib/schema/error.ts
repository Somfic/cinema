// Shape of the error body returned by the backend on a non-2xx RPC response.
// Matches `src/rpc.rs::RpcResponse::into_response`.
export interface RpcErrorPayload {
	kind: string;
	message: string;
}

export class RpcError extends Error {
	payload: RpcErrorPayload;

	constructor(payload: RpcErrorPayload) {
		super(payload.message || payload.kind);
		this.name = "RpcError";
		this.payload = payload;
	}
}
