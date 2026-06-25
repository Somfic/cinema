import { api } from "$lib/api";
import type { MediaType } from "./schema";

export function imageUrl(path: string, size: string = "original"): string {
	// URL contract lives in the draad `#[raw]` schema; the catch-all carries
	// `{size}{path}` (path already begins with `/`).
	return api.urls.image(`${size}${path}`);
}

export async function getDetails(type: MediaType, id: number) {
	if (type === "movie") return api.media.movieDetails(id);
	return api.media.tvDetails(id);
}

export async function playStream(infoHash: string, fileIdx: number) {
	const res = await api.streams.start(infoHash, fileIdx, null);
	return { url: res.url, local: res.local };
}

export function formatBytes(bytes: number): string {
	if (bytes >= 1_099_511_627_776) return `${(bytes / 1_099_511_627_776).toFixed(2)} TB`;
	if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
	if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(0)} MB`;
	if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
	return `${bytes} B`;
}
