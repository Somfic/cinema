import { Api, type Rpc } from "$lib/schema";
import { call, listen } from "$lib/schema/rpc";

const rpc: Rpc = { call, listen };

export const api = new Api(rpc);
